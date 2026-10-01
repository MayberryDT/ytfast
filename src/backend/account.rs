//! Writes to the signed-in account (see `crate::account`). They are made
//! one at a time, in the order asked: a like then an unlike, or two moves
//! in a playlist, reach YouTube Music in that order. Each answer goes back
//! stamped with the app's operation number; a few seconds after a success
//! the pages it affects are asked for again, once YouTube Music shows the
//! change.

use serde_json::{Value, json};

use super::*;
use crate::account::{Done, Edit, Failure};
use crate::innertube::Edited;

/// YouTube Music lists most changes within ~1.5 s, subscriptions within ~3 s
/// (docs/integration.md § Verified facts).
const SETTLE: Duration = Duration::from_secs(3);

/// One account write: its operation number, the edit, and the pages to
/// fetch again once it shows.
pub(super) struct Write {
    op: u64,
    edit: Edit,
    refresh: Vec<Target>,
}

impl super::Worker {
    pub(super) fn account_edit(&mut self, op: u64, edit: Edit, refresh: Vec<Target>) {
        let writes = self.account_writes.get_or_insert_with(|| {
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(writer(
                self.client.clone(),
                self.sink.clone(),
                self.internal_tx.clone(),
                rx,
            ));
            tx
        });
        let _ = writes.send(Write { op, edit, refresh });
    }

    /// The account's rating of a song, fetched fresh.
    pub(super) fn like_status(&self, video_id: String) {
        let client = self.client.clone();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            match client.like_status(&video_id).await {
                Ok(Some(like)) => sink.send(Event::Likes(vec![like])),
                Ok(None) => log::warn!("no rating in watch-next for {video_id}"),
                Err(error) => log::warn!("rating of {video_id}: {error}"),
            }
        });
    }
}

/// Makes the account writes one after another.
async fn writer(
    client: Arc<Client>,
    sink: Sink,
    internal: mpsc::UnboundedSender<Internal>,
    mut writes: mpsc::UnboundedReceiver<Write>,
) {
    while let Some(Write { op, edit, refresh }) = writes.recv().await {
        let started = Instant::now();
        let what = format!("{edit:?}");
        let result = match run(&client, edit).await {
            Ok(done) => Ok(done),
            Err(Answer::Api(ApiError::Auth)) => {
                let _ = internal.send(Internal::AuthFailed);
                Err(Failure::SignedOut)
            }
            Err(Answer::Api(ApiError::Offline(_))) => Err(Failure::Offline),
            Err(Answer::Api(error)) => Err(Failure::Refused(error.to_string())),
            Err(Answer::Refused(message)) if message.contains("already in") => {
                Err(Failure::AlreadyInPlaylist)
            }
            Err(Answer::Refused(message)) => Err(Failure::Refused(message)),
        };
        log::info!(
            "account write {op} {what}: {} after {:.2}s",
            match &result {
                Ok(_) => "accepted".to_owned(),
                Err(failure) => format!("{failure:?}"),
            },
            started.elapsed().as_secs_f64()
        );
        let ok = result.is_ok();
        sink.send(Event::AccountEdited { op, result });
        // The refetch waits for YouTube Music to list the change; the next
        // write doesn't wait for it.
        if ok && !refresh.is_empty() {
            let sink = sink.clone();
            tokio::spawn(async move {
                tokio::time::sleep(SETTLE).await;
                sink.send(Event::AccountRefresh(refresh));
            });
        }
    }
}

enum Answer {
    Api(ApiError),
    Refused(String),
}

impl From<ApiError> for Answer {
    fn from(error: ApiError) -> Self {
        Answer::Api(error)
    }
}

fn edited(answer: Edited) -> Result<Value, Answer> {
    match answer {
        Edited::Done(value) => Ok(value),
        Edited::Refused(message) => Err(Answer::Refused(message)),
    }
}

async fn run(client: &Client, edit: Edit) -> Result<Done, Answer> {
    match edit {
        Edit::Rate { video_id, status } => client.rate(&video_id, status).await?,
        Edit::Save { playlist_id, save } => client.save_to_library(&playlist_id, save).await?,
        Edit::Subscribe {
            channel_id,
            subscribe,
        } => client.subscribe(&channel_id, subscribe).await?,
        Edit::Create {
            title,
            description,
            video_ids,
        } => {
            let id = client
                .create_playlist(&title, &description, &video_ids)
                .await?;
            return Ok(Done::Created(id));
        }
        Edit::Add {
            playlist_id,
            video_ids,
        } => {
            // Without a dedupe option YouTube Music refuses a song already
            // in the playlist ("This track is already in the playlist");
            // `DEDUPE_OPTION_SKIP` would add it a second time.
            let actions = video_ids
                .iter()
                .map(|id| json!({ "action": "ACTION_ADD_VIDEO", "addedVideoId": id }))
                .collect();
            let value = edited(client.edit_playlist(&playlist_id, actions).await?)?;
            let added = value
                .get("playlistEditResults")
                .and_then(Value::as_array)
                .map(|results| {
                    results
                        .iter()
                        .filter_map(|r| {
                            let data = r.get("playlistEditVideoAddedResultData")?;
                            Some((
                                data.get("videoId")?.as_str()?.to_owned(),
                                data.get("setVideoId")?.as_str()?.to_owned(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();
            return Ok(Done::Added(added));
        }
        Edit::Remove {
            playlist_id,
            video_id,
            set_video_id,
        } => {
            let action = json!({ "action": "ACTION_REMOVE_VIDEO", "setVideoId": set_video_id, "removedVideoId": video_id });
            edited(client.edit_playlist(&playlist_id, vec![action]).await?)?;
        }
        Edit::Move {
            playlist_id,
            set_video_id,
            before,
        } => {
            let mut action =
                json!({ "action": "ACTION_MOVE_VIDEO_BEFORE", "setVideoId": set_video_id });
            if let Some(before) = before {
                action["movedSetVideoIdSuccessor"] = json!(before);
            }
            edited(client.edit_playlist(&playlist_id, vec![action]).await?)?;
        }
        Edit::Details {
            playlist_id,
            title,
            description,
        } => {
            let mut actions = Vec::new();
            if let Some(title) = title {
                actions
                    .push(json!({ "action": "ACTION_SET_PLAYLIST_NAME", "playlistName": title }));
            }
            if let Some(description) = description {
                actions.push(json!({ "action": "ACTION_SET_PLAYLIST_DESCRIPTION", "playlistDescription": description }));
            }
            edited(client.edit_playlist(&playlist_id, actions).await?)?;
        }
        Edit::Delete { playlist_id } => client.delete_playlist(&playlist_id).await?,
    }
    Ok(Done::Ok)
}
