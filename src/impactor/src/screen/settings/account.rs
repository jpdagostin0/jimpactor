use std::collections::HashMap;
use std::time::Duration;

use iced::widget::{button, column, pick_list, row, scrollable, text};
use iced::{Element, Fill, Task};
use plume_store::AccountStore;
use rust_i18n::t;

use super::{Message, Team};
use crate::{appearance, subscriptions};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Checking(u64),
    Connected,
    Failed(String),
}

#[derive(Debug, Default)]
pub(super) struct AccountTab {
    teams: HashMap<String, Vec<Team>>,
    status: HashMap<String, Status>,
    next_request: u64,
}

impl AccountTab {
    pub fn forget(&mut self, email: &str) {
        self.teams.remove(email);
        self.status.remove(email);
    }

    pub fn check(&mut self, email: String, store: Option<&AccountStore>) -> Task<Message> {
        let Some(account) = store.and_then(|store| store.get_account(&email)).cloned() else {
            return Task::none();
        };
        if matches!(self.status.get(&email), Some(Status::Checking(_))) {
            return Task::none();
        }
        if account.xcode_gs_token().is_empty() {
            self.status.insert(
                email,
                Status::Failed(t!("account_session_missing").to_string()),
            );
            return Task::none();
        }
        self.next_request += 1;
        let request = self.next_request;
        self.status.insert(email.clone(), Status::Checking(request));
        Task::perform(
            subscriptions::run_on_tokio(async move {
                tokio::time::timeout(
                    Duration::from_secs(30),
                    subscriptions::fetch_teams(&account),
                )
                .await
                .unwrap_or_else(|_| Err(t!("account_check_timeout").to_string()))
            }),
            move |result| Message::TeamsLoaded(email.clone(), request, result),
        )
    }

    pub fn checked(&mut self, email: String, request: u64, result: Result<Vec<Team>, String>) {
        if self.status.get(&email) != Some(&Status::Checking(request)) {
            return;
        }
        match result {
            Ok(teams) => {
                self.teams.insert(email.clone(), teams);
                self.status.insert(email, Status::Connected);
            }
            Err(error) => {
                self.status.insert(email, Status::Failed(error));
            }
        }
    }

    pub fn view<'a>(&'a self, store: Option<&'a AccountStore>) -> Element<'a, Message> {
        let mut content = column![
            text(t!("account_title")).size(16),
            text(t!("account_description")),
        ]
        .spacing(appearance::THEME_PADDING);

        let Some(store) = store else {
            return content.push(text(t!("settings_loading_accounts"))).into();
        };
        let mut emails: Vec<_> = store.accounts().keys().cloned().collect();
        emails.sort();
        let selected = store.selected_account();
        if !emails.is_empty() {
            content = content.push(
                pick_list(
                    emails,
                    selected.map(|account| account.email().clone()),
                    Message::SelectAccount,
                )
                .placeholder(t!("account_choose"))
                .style(appearance::s_pick_list)
                .width(Fill),
            );
        }

        if let Some(account) = selected {
            let email = account.email();
            if !account.first_name().is_empty() {
                content = content.push(text(account.first_name()));
            }
            content = content.push(text(email));
            let status = self.status.get(email);
            let checking = matches!(status, Some(Status::Checking(_)));
            let label = match status {
                Some(Status::Checking(_)) => t!("account_checking"),
                Some(Status::Connected) => t!("account_connected"),
                Some(Status::Failed(_)) => t!("account_check_failed"),
                None if account.xcode_gs_token().is_empty() => t!("account_session_missing"),
                None => t!("account_saved"),
            };
            content = content.push(text(label));
            if let Some(Status::Failed(error)) = status {
                content = content.push(text(error).size(appearance::THEME_FONT_SIZE - 1.0));
            }
            let teams = self.teams.get(email).cloned().unwrap_or_default();
            let team = teams
                .iter()
                .find(|team| &team.id == account.team_id())
                .cloned();
            let placeholder = if checking {
                t!("settings_loading_teams").to_string()
            } else if !account.team_id().is_empty() {
                account.team_id().clone()
            } else {
                t!("settings_select_teams").to_string()
            };
            let team_email = email.clone();
            content = content.push(text(t!("account_team"))).push(
                pick_list(teams, team, move |team: Team| {
                    Message::SelectTeam(team_email.clone(), team.id)
                })
                .placeholder(placeholder)
                .on_open(Message::FetchTeams(email.clone()))
                .style(appearance::s_pick_list)
                .width(Fill),
            );
            content = content.push(
                row![
                    button(text(t!("account_check_status")))
                        .on_press_maybe((!checking).then(|| Message::FetchTeams(email.clone())))
                        .style(appearance::s_button)
                        .width(Fill),
                    button(text(t!("account_sign_in_again")))
                        .on_press(Message::ShowLogin(Some(email.clone())))
                        .style(appearance::s_button)
                        .width(Fill),
                ]
                .spacing(appearance::THEME_PADDING),
            );
            content = content.push(
                row![
                    button(text(t!("settings_export_p12")))
                        .on_press_maybe(
                            (!checking && !account.xcode_gs_token().is_empty())
                                .then_some(Message::ExportP12)
                        )
                        .style(appearance::s_button)
                        .width(Fill),
                    button(text(t!("settings_remove_account")))
                        .on_press(Message::RemoveAccount(email.clone()))
                        .style(appearance::s_button)
                        .width(Fill),
                ]
                .spacing(appearance::THEME_PADDING),
            );
        } else {
            content = content.push(text(t!("account_signed_out")));
        }
        content = content.push(
            button(text(if store.accounts().is_empty() {
                t!("account_sign_in")
            } else {
                t!("settings_add_account")
            }))
            .on_press(Message::ShowLogin(None))
            .style(appearance::p_button),
        );
        scrollable(
            content
                .padding([0.0, appearance::THEME_PADDING])
                .width(Fill),
        )
        .height(Fill)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_checks_cannot_replace_a_new_session_or_restore_a_removed_account() {
        let email = "test@example.com".to_string();
        let mut tab = AccountTab::default();
        tab.status.insert(email.clone(), Status::Checking(2));
        tab.checked(email.clone(), 1, Err("old session".into()));
        assert_eq!(tab.status.get(&email), Some(&Status::Checking(2)));
        tab.checked(email.clone(), 2, Ok(Vec::new()));
        assert_eq!(tab.status.get(&email), Some(&Status::Connected));
        tab.forget(&email);
        tab.checked(email.clone(), 2, Ok(Vec::new()));
        assert!(!tab.status.contains_key(&email));
    }

    #[test]
    fn failed_checks_keep_the_error_instead_of_claiming_the_account_is_connected() {
        let mut tab = AccountTab::default();
        tab.status
            .insert("test@example.com".into(), Status::Checking(1));
        tab.checked(
            "test@example.com".into(),
            1,
            Err("Apple is unreachable".into()),
        );
        assert_eq!(
            tab.status.get("test@example.com"),
            Some(&Status::Failed("Apple is unreachable".into()))
        );
    }
}
