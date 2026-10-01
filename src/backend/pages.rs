use super::*;

impl super::Worker {
    pub(super) fn load_page(&self, target: Target, seq: u64) {
        let key = target.key();
        let client = self.client.clone();
        let sink = self.sink.clone();
        let path = self.paths.page_file(&key);
        let internal = self.internal_tx.clone();
        tokio::spawn(async move {
            if let Ok(bytes) = tokio::fs::read(&path).await
                && let Ok(page) = serde_json::from_slice::<Page>(&bytes)
            {
                sink.send(Event::Page {
                    key: key.clone(),
                    seq,
                    result: Ok(Box::new(page)),
                    cached: true,
                });
            }
            let result = match &target {
                Target::Browse { id, params } => client.browse(id, params.as_deref()).await,
                Target::Search { query, params } => client.search(query, params.as_deref()).await,
                Target::Watch { .. } => Err(ApiError::Invalid("not a page".into())),
            };
            match result {
                Ok(value) => {
                    let page = parse::page(&value);
                    if let Ok(bytes) = serde_json::to_vec(&page) {
                        let _ = crate::paths::write_atomic(&path, &bytes);
                    }
                    sink.send(Event::Page {
                        key,
                        seq,
                        result: Ok(Box::new(page)),
                        cached: false,
                    });
                }
                Err(error) => {
                    if matches!(error, ApiError::Auth) {
                        let _ = internal.send(Internal::AuthFailed);
                    }
                    sink.send(Event::Page {
                        key,
                        seq,
                        result: Err(error.to_string()),
                        cached: false,
                    });
                }
            }
        });
    }
}
