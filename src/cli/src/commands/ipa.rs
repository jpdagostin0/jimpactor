use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use plume_utils::inspect::inspect_ipa;

#[derive(Debug, Args)]
pub struct IpaArgs {
    #[arg(value_name = "IPA")]
    pub path: PathBuf,
    /// Print the full report as JSON
    #[arg(long)]
    pub json: bool,
}

pub fn execute(args: IpaArgs) -> Result<()> {
    let report = inspect_ipa(&args.path)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let text = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".into());
    let line = |label: &str, value: String| println!("{label:<16}{value}");
    line("Name", text(&report.name));
    line("Bundle ID", text(&report.bundle_id));
    line(
        "Version",
        format!("{} ({})", text(&report.version), text(&report.build)),
    );
    line("Minimum iOS", text(&report.minimum_os));
    line("Devices", report.families.join(", "));
    line("Architectures", report.architectures.join(", "));
    line(
        "Encrypted",
        report
            .encrypted
            .map_or("unknown".into(), |encrypted| encrypted.to_string()),
    );
    line(
        "Size",
        format!(
            "{} bytes ({} unpacked, {} files)",
            report.archive_size, report.unpacked_size, report.files
        ),
    );
    if let Some(profile) = &report.profile {
        let expires = profile.expiration.map_or("-".to_owned(), |date| {
            chrono::DateTime::<chrono::Utc>::from(date)
                .format("%Y-%m-%d")
                .to_string()
        });
        line(
            "Profile",
            format!(
                "{} ({}), expires {expires}",
                profile.name,
                profile.team.as_deref().unwrap_or("-")
            ),
        );
    }
    let list = |label: &str, values: &[String]| {
        if !values.is_empty() {
            println!("{label}:");
            for value in values {
                println!("  {value}");
            }
        }
    };
    let extensions: Vec<String> = report
        .extensions
        .iter()
        .map(|extension| {
            format!(
                "{} ({})",
                extension.name,
                extension.bundle_id.as_deref().unwrap_or("-")
            )
        })
        .collect();
    let permissions: Vec<String> = report
        .permissions
        .iter()
        .map(|(key, reason)| format!("{key}: {reason}"))
        .collect();
    list("Extensions", &extensions);
    list("Frameworks", &report.frameworks);
    list("Linked libraries", &report.linked_libraries);
    list("URL schemes", &report.url_schemes);
    list("Permissions", &permissions);
    list("Entitlements", &report.entitlements);
    list("Warnings", &report.warnings);
    Ok(())
}
