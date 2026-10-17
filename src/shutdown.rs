use std::collections::HashSet;
use std::time::Duration;

use rustle_core::{AppEvent, EventReceiver, EventSender};

pub(crate) async fn wait(sender: EventSender, mut receiver: EventReceiver) -> anyhow::Result<()> {
    let mut pending = HashSet::new();
    let mut quitting = false;
    let deadline = tokio::time::sleep(Duration::from_secs(600));
    tokio::pin!(deadline);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Ok(AppEvent::MeetingStarted { id, .. }) => { pending.insert(id); }
                Ok(AppEvent::TranscriptImported { meeting_id, .. }) => { pending.insert(meeting_id); }
                Ok(AppEvent::MeetingFinished { meeting_id }) => { pending.remove(&meeting_id); }
                Ok(AppEvent::QuitRequested) if !quitting => {
                    quitting = true;
                    deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(600));
                    for id in &pending { let _ = sender.send(AppEvent::MeetingEnded { id: *id }); }
                }
                Ok(_) => {},
                Err(error) => anyhow::bail!("shutdown coordinator lost workflow events: {error}; recordings retained"),
            },
            _ = tokio::signal::ctrl_c(), if !quitting => { let _ = sender.send(AppEvent::QuitRequested); }
            _ = terminate.recv(), if !quitting => { let _ = sender.send(AppEvent::QuitRequested); }
            _ = &mut deadline, if quitting => {
                anyhow::bail!("timed out draining {} meetings; recordings/drafts retained", pending.len());
            }
        }
        if quitting && pending.is_empty() {
            let _ = sender.send(AppEvent::ShutdownComplete);
            tracing::info!("shutdown complete; pending meetings drained");
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quit_waits_for_processing_completion() {
        let bus = rustle_core::EventBus::new();
        let mut observer = bus.subscribe();
        let task = tokio::spawn(wait(bus.sender(), bus.subscribe()));
        let id = uuid::Uuid::new_v4();
        bus.publish(AppEvent::MeetingStarted {
            id,
            name: "test".to_owned(),
            source: rustle_core::DetectionSource::Manual,
        })
        .unwrap();
        bus.publish(AppEvent::QuitRequested).unwrap();
        loop {
            if matches!(observer.recv().await.unwrap(), AppEvent::MeetingEnded { id: seen } if seen == id)
            {
                break;
            }
        }
        assert!(!task.is_finished());
        bus.publish(AppEvent::MeetingFinished { meeting_id: id })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
