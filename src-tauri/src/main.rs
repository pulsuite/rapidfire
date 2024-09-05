// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod msgbox;
pub mod volume;

use serde::{Deserialize, Serialize};
use tauri::Manager;
use tokio::fs;
use tokio::sync::{mpsc, oneshot};

use self::volume::volume;

enum Event {
    VolumeWarning { is_full: bool },
    Project { project: Project },
}

enum ProjectMessage {
    GetProject {
        tx: oneshot::Sender<Project>,
    },
    PatchSoundVolume {
        scene_id: String,
        sound_id: String,
        volume: u32,
    },
    PatchSoundLooped {
        scene_id: String,
        sound_id: String,
        looped: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SoundVariant {
    Bgm,
    Se,
    Voice,
}

struct RapidFireState {
    project_tx: mpsc::Sender<ProjectMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PatchSoundVolume {
    scene_id: String,
    sound_id: String,
    volume: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PatchSoundLooped {
    scene_id: String,
    sound_id: String,
    looped: bool,
}

#[derive(Debug, Clone, Serialize)]
struct VolumeWarningPayload {
    is_full: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SoundInstance {
    id: String,
    display_name: String,
    path: String,
    volume: u32,
    looped: bool,
    variant: SoundVariant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SoundScene {
    id: String,
    display_name: String,
    sounds: Vec<SoundInstance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Project {
    display_name: String,
    scenes: Vec<SoundScene>,
}

#[tauri::command]
fn get_volume_warning() -> VolumeWarningPayload {
    VolumeWarningPayload {
        is_full: volume().map(|v| v >= 0.995).unwrap_or(true),
    }
}

#[tauri::command]
async fn patch_sound_volume(
    state: tauri::State<'_, RapidFireState>,
    payload: PatchSoundVolume,
) -> Result<(), ()> {
    state
        .project_tx
        .send(ProjectMessage::PatchSoundVolume {
            scene_id: payload.scene_id,
            sound_id: payload.sound_id,
            volume: payload.volume,
        })
        .await
        .unwrap();
    Ok(())
}

#[tauri::command]
async fn patch_sound_looped(
    state: tauri::State<'_, RapidFireState>,
    payload: PatchSoundLooped,
) -> Result<(), ()> {
    state
        .project_tx
        .send(ProjectMessage::PatchSoundLooped {
            scene_id: payload.scene_id,
            sound_id: payload.sound_id,
            looped: payload.looped,
        })
        .await
        .unwrap();
    Ok(())
}

#[tauri::command]
async fn get_project(state: tauri::State<'_, RapidFireState>) -> Result<Project, ()> {
    let (tx, rx) = oneshot::channel();
    state
        .project_tx
        .send(ProjectMessage::GetProject { tx })
        .await
        .unwrap();
    rx.await.map_err(|_| ())
}

#[tokio::main]
async fn main() {
    let (event_tx_original, mut event_rx) = mpsc::channel(32);
    let (project_tx, mut project_rx) = mpsc::channel(32);

    let app = tauri::Builder::default()
        .setup(|app| {
            let app_handle = app.handle();
            tokio::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    match event {
                        Event::VolumeWarning { is_full } => {
                            app_handle
                                .emit_all("volume_warning", VolumeWarningPayload { is_full })
                                .expect("failed to emit event");
                        }
                        Event::Project { project } => {
                            app_handle
                                .emit_all("project", project)
                                .expect("failed to emit event");
                        }
                    }
                }
            });

            Ok(())
        })
        .manage(RapidFireState {
            project_tx: project_tx.clone(),
        })
        .invoke_handler(tauri::generate_handler![
            get_volume_warning,
            get_project,
            patch_sound_volume,
            patch_sound_looped
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    if let Some(mut rx) = volume::receive_volume_change().await {
        let event_tx = event_tx_original.clone();
        tokio::spawn(async move {
            let mut last_is_full = false;
            loop {
                let volume = *rx.borrow_and_update();
                let now_is_full = volume >= 0.995;
                if now_is_full != last_is_full {
                    event_tx
                        .send(Event::VolumeWarning {
                            is_full: now_is_full,
                        })
                        .await
                        .unwrap();
                    last_is_full = now_is_full;
                }
                if rx.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    let event_tx = event_tx_original.clone();
    tokio::spawn(async move {
        let project = fs::read_to_string("projects/index.json")
            .await
            .expect("failed to read project");
        let mut project =
            serde_json::from_str::<Project>(&project).expect("failed to parse project");

        let update = |event_tx: mpsc::Sender<Event>, project: Project| async move {
            event_tx
                .clone()
                .send(Event::Project {
                    project: project.clone(),
                })
                .await
                .expect("failed to send project refresh");
            let text = serde_json::to_string_pretty(&project).expect("failed to serialize project");
            fs::write("projects/index.json", text)
                .await
                .expect("failed to write project");
        };

        while let Some(message) = project_rx.recv().await {
            match message {
                ProjectMessage::GetProject { tx } => {
                    tx.send(project.clone()).unwrap();
                }
                ProjectMessage::PatchSoundVolume {
                    scene_id,
                    sound_id,
                    volume,
                } => {
                    if let Some(scene) =
                        project.scenes.iter_mut().find(|scene| scene.id == scene_id)
                    {
                        if let Some(sound) =
                            scene.sounds.iter_mut().find(|sound| sound.id == sound_id)
                        {
                            sound.volume = volume;
                        }
                    }
                    update(event_tx.clone(), project.clone()).await;
                }
                ProjectMessage::PatchSoundLooped {
                    scene_id,
                    sound_id,
                    looped,
                } => {
                    if let Some(scene) =
                        project.scenes.iter_mut().find(|scene| scene.id == scene_id)
                    {
                        if let Some(sound) =
                            scene.sounds.iter_mut().find(|sound| sound.id == sound_id)
                        {
                            sound.looped = looped;
                        }
                    }
                    update(event_tx.clone(), project.clone()).await;
                }
            }
        }

        println!("{:?}", project);
    });

    app.run(|_app_handle, _webview| {});
}
