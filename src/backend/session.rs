use super::*;

impl super::Worker {
    pub(super) fn connect(&mut self) {
        self.last_connect = Some(Instant::now());
        self.sink.send(Event::Account(Account::Checking));
        let client = self.client.clone();
        let resolver = self.resolver.clone();
        let paths = self.paths.clone();
        let tx = self.internal_tx.clone();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            let scratch = paths.runtime.clone();
            let preferred = crate::settings::Settings::load(&paths).browser_profile;
            let loaded = tokio::task::spawn_blocking(move || {
                let session = crate::auth::load(&scratch, preferred.as_deref());
                (session, crate::auth::profiles(&scratch))
            })
            .await
            .map(|(session, profiles)| {
                let current = session.as_ref().ok().map(|s| s.profile.clone());
                sink.send(Event::Profiles {
                    list: profiles,
                    current,
                });
                session
            });
            let session = match loaded {
                Ok(Ok(session)) => session,
                Ok(Err(error)) => {
                    client.set_session(None);
                    resolver.set_cookie_file(None);
                    let _ = tx.send(Internal::Connected(Account::SignedOut {
                        reason: format!("{error:#}"),
                    }));
                    return;
                }
                Err(error) => {
                    let _ = tx.send(Internal::Connected(Account::SignedOut {
                        reason: error.to_string(),
                    }));
                    return;
                }
            };
            let source = session.source.clone();
            let cookie_file = paths.cookie_file();
            match session.write_netscape(&cookie_file) {
                Ok(()) => resolver.set_cookie_file(Some(cookie_file)),
                Err(error) => log::warn!("could not write the cookie file: {error:#}"),
            }
            client.set_session(Some(session));
            let account = match client.account().await {
                Ok(value) => match parse::account(&value) {
                    Some((name, photo)) => Account::SignedIn {
                        name,
                        photo,
                        source,
                    },
                    None => {
                        client.set_session(None);
                        resolver.set_cookie_file(None);
                        Account::SignedOut {
                            reason: format!("{source} isn't signed in to YouTube Music"),
                        }
                    }
                },
                Err(ApiError::Auth) => {
                    client.set_session(None);
                    resolver.set_cookie_file(None);
                    Account::SignedOut {
                        reason: format!("The YouTube session in {source} has expired"),
                    }
                }
                Err(error) => Account::Unverified {
                    reason: error.to_string(),
                },
            };
            let _ = tx.send(Internal::Connected(account));
        });
    }
}
