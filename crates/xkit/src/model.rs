use crate::{Result, entitlements::Entitlements, invalid};
use plist::{Dictionary, Value};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use x509_certificate::CapturedX509Certificate;

/// Validated, DER encoded X.509 certificate, serialized as plist data.
#[derive(Clone, Debug)]
pub struct Certificate(CapturedX509Certificate);

impl Certificate {
    pub fn from_der(data: impl AsRef<[u8]>) -> Result<Self> {
        Ok(Self(CapturedX509Certificate::from_der(
            data.as_ref().to_vec(),
        )?))
    }
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_der(fs::read(path)?)
    }
    pub fn data(&self) -> &[u8] {
        self.0.constructed_data()
    }
    pub fn raw(&self) -> &CapturedX509Certificate {
        &self.0
    }
    pub fn developer_identity(&self) -> Result<String> {
        self.0
            .subject_common_name()
            .ok_or_else(|| invalid("Certificate has no common name"))
    }
    pub fn team_id(&self) -> Result<String> {
        self.0
            .subject_name()
            .iter_organizational_unit()
            .next()
            .and_then(|v| v.to_string().ok())
            .ok_or_else(|| invalid("Certificate has no team ID"))
    }
    pub fn serial_number(&self) -> String {
        // Keep leading zero nibbles, but drop sign-padding bytes.
        let serial = self.0.serial_number_asn1().clone();
        let bytes = serial.as_slice();
        let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
        hex::encode_upper(&bytes[start..])
    }
    pub fn has_serial_number(&self, serial: &str) -> bool {
        self.serial_number()
            .trim_start_matches('0')
            .eq_ignore_ascii_case(serial.trim_start_matches('0'))
    }
    pub fn was_issued_before(&self, other: &Self) -> bool {
        self.0.validity_not_before() < other.0.validity_not_before()
    }
}

impl Serialize for Certificate {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.data())
    }
}
impl<'de> Deserialize<'de> for Certificate {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let data = plist::Data::deserialize(deserializer)?;
        Self::from_der(data.as_ref()).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateKey {
    pub data: plist::Data,
}

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateKey([redacted])")
    }
}

#[derive(Clone, Debug)]
pub struct CSR {
    pub pem_string: String,
}

pub struct Keypair(rsa::RsaPrivateKey);
impl Keypair {
    pub fn new() -> Result<Self> {
        Ok(Self(
            rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048)
                .map_err(|e| invalid(e.to_string()))?,
        ))
    }
    pub fn private_key(&self) -> Result<PrivateKey> {
        use rsa::pkcs8::EncodePrivateKey;
        Ok(PrivateKey {
            data: self
                .0
                .to_pkcs8_pem(Default::default())
                .map_err(|e| invalid(e.to_string()))?
                .as_bytes()
                .to_vec()
                .into(),
        })
    }
    pub fn generate_csr(&self) -> Result<CSR> {
        use rsa::pkcs8::EncodePrivateKey;
        let der = self.0.to_pkcs8_der().map_err(|e| invalid(e.to_string()))?;
        let mut params = rcgen::CertificateParams::new(Vec::new());
        params.alg = &rcgen::PKCS_RSA_SHA256;
        params.key_pair =
            Some(rcgen::KeyPair::from_der(der.as_bytes()).map_err(|e| invalid(e.to_string()))?);
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CountryName, "US");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "xtool");
        let pem_string = rcgen::Certificate::from_params(params)
            .and_then(|c| c.serialize_request_pem())
            .map_err(|e| invalid(e.to_string()))?;
        Ok(CSR { pem_string })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SigningInfo {
    pub private_key: PrivateKey,
    pub certificate: Certificate,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MobileprovisionDigest {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "UUID")]
    pub uuid: String,
    #[serde(rename = "TeamIdentifier")]
    pub team_identifiers: Vec<String>,
    #[serde(rename = "CreationDate")]
    pub creation_date: plist::Date,
    #[serde(rename = "ExpirationDate")]
    pub expiration_date: plist::Date,
    #[serde(rename = "ProvisionedDevices", default)]
    pub devices: Vec<String>,
    #[serde(rename = "DeveloperCertificates")]
    pub certificates: Vec<Certificate>,
    #[serde(rename = "Entitlements")]
    pub entitlements: Entitlements,
}

#[derive(Clone, Debug)]
pub struct Mobileprovision {
    data: Vec<u8>,
    digest: MobileprovisionDigest,
}

impl Mobileprovision {
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        // Apple's CMS payload contains an XML plist. Also accept a standalone
        // plist (including binary plists) for tooling and offline fixtures.
        let value = match Value::from_reader(std::io::Cursor::new(&data)) {
            Ok(value) => value,
            Err(_) => {
                let start = data
                    .windows(6)
                    .position(|v| v == b"<plist")
                    .ok_or_else(|| invalid("Invalid provisioning profile"))?;
                let end = data
                    .windows(8)
                    .rposition(|v| v == b"</plist>")
                    .map(|v| v + 8)
                    .filter(|v| *v > start)
                    .ok_or_else(|| invalid("Invalid provisioning profile"))?;
                Value::from_reader_xml(&data[start..end])?
            }
        };
        let digest = plist::from_value(&value)?;
        Ok(Self { data, digest })
    }
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_bytes(fs::read(path)?)
    }
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn digest(&self) -> &MobileprovisionDigest {
        &self.digest
    }
    pub fn entitlements(&self) -> &Dictionary {
        &self.digest.entitlements.0
    }
}
