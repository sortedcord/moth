//! Account sign-in and sign-out, for the settings page's accounts section and the browser
//! extension's "share account" button.

use gpui::{Context, Task, Window};

use super::{AppView, BackendHandle};
use crate::backend::accounts::{AccountInfo, DeviceCode, Role};

/// Where an account's sign-in stands, as shown on the settings page.
#[derive(Clone, Debug, PartialEq)]
pub enum AccountState {
    SignedOut,
    SignedIn(AccountInfo),
    /// Waiting for the user to enter the code.
    DeviceCode(DeviceCode),
    /// The cookie field is open.
    PastingCookies,
    /// Talking to the site, e.g. "Getting a code…".
    Working(&'static str),
}

pub struct Account {
    pub state: AccountState,
    pub error: Option<String>,
    _task: Option<Task<()>>,
}

pub struct Accounts {
    accounts: [Account; 2],
}

impl Accounts {
    pub fn new(cx: &mut Context<AppView>) -> Self {
        let backend = cx.global::<BackendHandle>().0.clone();
        let account = |role| Account { state: state_of(backend.account(role)), error: None, _task: None };
        Self { accounts: [account(Role::Playback), account(Role::Recommendations)] }
    }

    pub fn get(&self, role: Role) -> &Account {
        &self.accounts[index(role)]
    }

    fn get_mut(&mut self, role: Role) -> &mut Account {
        &mut self.accounts[index(role)]
    }

    /// The role whose cookie field is open, if any.
    pub fn pasting(&self) -> Option<Role> {
        Role::ALL.into_iter().find(|role| self.get(*role).state == AccountState::PastingCookies)
    }
}

fn index(role: Role) -> usize {
    match role {
        Role::Playback => 0,
        Role::Recommendations => 1,
    }
}

fn state_of(info: Option<AccountInfo>) -> AccountState {
    info.map_or(AccountState::SignedOut, AccountState::SignedIn)
}

impl AppView {
    /// At startup: the keyring only says who's signed in, so look up their names.
    pub(super) fn refresh_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let refresh = cx.global::<BackendHandle>().0.refresh_accounts();
        cx.spawn_in(window, async move |this, cx| {
            let signed_out = refresh.await.unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                let backend = cx.global::<BackendHandle>().0.clone();
                for role in Role::ALL {
                    let account = this.accounts.get_mut(role);
                    if matches!(account.state, AccountState::SignedIn(_)) {
                        account.state = state_of(backend.account(role));
                    }
                }
                for (role, why) in signed_out {
                    this.accounts.get_mut(role).error = Some(why);
                    if role == Role::Recommendations {
                        this.reload_feed(window, cx);
                        this.music_home_changed(window, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn sign_in_with_code(&mut self, role: Role, window: &mut Window, cx: &mut Context<Self>) {
        let backend = cx.global::<BackendHandle>().0.clone();
        let begin = backend.begin_device_sign_in(role);
        let account = self.accounts.get_mut(role);
        account.state = AccountState::Working("Getting a code…");
        account.error = None;
        account._task = Some(cx.spawn_in(window, async move |this, cx| {
            let code = match begin.await {
                Ok(code) => code,
                Err(err) => return this.update(cx, |this, cx| this.account_failed(role, err, cx)).unwrap_or(()),
            };
            let finish = this.update(cx, |this, cx| {
                this.accounts.get_mut(role).state = AccountState::DeviceCode(code);
                cx.notify();
                cx.global::<BackendHandle>().0.finish_device_sign_in(role)
            });
            let Ok(finish) = finish else { return };
            let result = finish.await;
            this.update_in(cx, |this, window, cx| this.account_done(role, result, window, cx)).ok();
        }));
        cx.notify();
    }

    pub(super) fn open_cookie_field(&mut self, role: Role, window: &mut Window, cx: &mut Context<Self>) {
        for other in Role::ALL {
            if self.accounts.get(other).state == AccountState::PastingCookies {
                self.cancel_sign_in(other, cx);
            }
        }
        let account = self.accounts.get_mut(role);
        account.state = AccountState::PastingCookies;
        account.error = None;
        self.cookie_input.update(cx, |input, cx| {
            input.set_text("", window, cx);
            input.focus_and_select_all(window, cx);
        });
        cx.notify();
    }

    /// Sign in with a browser session's cookies: pasted in the field, or sent by the extension.
    pub(super) fn sign_in_with_cookies(
        &mut self,
        role: Role,
        cookies: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sign_in = cx.global::<BackendHandle>().0.sign_in_with_cookies(role, cookies);
        let account = self.accounts.get_mut(role);
        account.state = AccountState::Working("Checking the session…");
        account.error = None;
        account._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = sign_in.await;
            this.update_in(cx, |this, window, cx| this.account_done(role, result, window, cx)).ok();
        }));
        cx.notify();
    }

    pub(super) fn cancel_sign_in(&mut self, role: Role, cx: &mut Context<Self>) {
        let backend = cx.global::<BackendHandle>().0.clone();
        backend.cancel_device_sign_in(role);
        let account = self.accounts.get_mut(role);
        account._task = None;
        account.state = state_of(backend.account(role));
        cx.notify();
    }

    pub(super) fn sign_out(&mut self, role: Role, window: &mut Window, cx: &mut Context<Self>) {
        let sign_out = cx.global::<BackendHandle>().0.sign_out(role);
        let account = self.accounts.get_mut(role);
        account.state = AccountState::Working("Signing out…");
        account.error = None;
        account._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = sign_out.await;
            this.update_in(cx, |this, window, cx| {
                if let Err(err) = result {
                    this.accounts.get_mut(role).error = Some(format!("{err:#}"));
                }
                this.account_changed(role, window, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    fn account_done(
        &mut self,
        role: Role,
        result: anyhow::Result<AccountInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(_) => self.account_changed(role, window, cx),
            Err(err) => self.account_failed(role, err, cx),
        }
    }

    fn account_failed(&mut self, role: Role, err: anyhow::Error, cx: &mut Context<Self>) {
        let backend = cx.global::<BackendHandle>().0.clone();
        let account = self.accounts.get_mut(role);
        account.state = state_of(backend.account(role));
        account.error = Some(format!("{err:#}"));
        cx.notify();
    }

    /// Someone signed in or out: show it, and refresh what depends on the account.
    fn account_changed(&mut self, role: Role, window: &mut Window, cx: &mut Context<Self>) {
        let info = cx.global::<BackendHandle>().0.account(role);
        self.accounts.get_mut(role).state = state_of(info);
        if role == Role::Recommendations {
            self.reload_feed(window, cx);
            self.music_home_changed(window, cx);
        }
        cx.notify();
    }
}
