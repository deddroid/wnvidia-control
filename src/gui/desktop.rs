//! Desktop integration: tray icon (StatusNotifierItem via `ksni`), single
//! instance (a socket in `$XDG_RUNTIME_DIR`) and XDG autostart.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::ipc::{self, Request};
use crate::profile::{Profile, ProfileStore};

/// Commands coming from the tray or from a second instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Show,
    Quit,
}

/// Menu labels in the user's language.
#[derive(Clone)]
pub struct Labels {
    pub show: String,
    pub quit: String,
    pub reset: String,
    pub tooltip: String,
}

// ---- single instance ----------------------------------------------------------

fn instance_socket() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("wnvctl-gui.sock")
}

/// Returns `false` if another instance is running (it has been asked to show
/// its window); otherwise starts listening for later launches.
pub fn claim_single_instance(tx: Sender<Cmd>) -> bool {
    use std::io::{BufRead, BufReader, Write};
    let path = instance_socket();
    if let Ok(mut conn) = UnixStream::connect(&path) {
        let _ = conn.write_all(b"show\n");
        return false;
    }
    // Nobody answered: a leftover socket from a crash.
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else {
        return true;
    };
    std::thread::Builder::new()
        .name("wnvctl-instance".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut line = String::new();
                let _ = BufReader::new(conn).read_line(&mut line);
                if line.trim() == "show" && tx.send(Cmd::Show).is_err() {
                    return;
                }
            }
        })
        .ok();
    true
}

// ---- tray ------------------------------------------------------------------------

/// RGBA pixels of the embedded tray icon.
fn icon_rgba() -> Option<(Vec<u8>, u32, u32)> {
    super::decode_png(super::TRAY_PNG)
}

pub fn spawn_tray(tx: Sender<Cmd>, labels: Labels) {
    use ksni::TrayMethods;

    struct Tray {
        tx: Sender<Cmd>,
        labels: Labels,
        icon: Vec<ksni::Icon>,
    }

    fn apply(profile: Profile) {
        // GPU 0: the tray is a shortcut for the common single-GPU case.
        std::thread::spawn(move || {
            let _ = ipc::call_done(&Request::Apply {
                gpu: "0".into(),
                profile,
            });
        });
    }

    impl ksni::Tray for Tray {
        fn id(&self) -> String {
            "wnvctl".into()
        }
        fn title(&self) -> String {
            self.labels.tooltip.clone()
        }
        fn icon_name(&self) -> String {
            "wnvctl".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            self.icon.clone()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: self.labels.tooltip.clone(),
                ..Default::default()
            }
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.tx.send(Cmd::Show);
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            let mut items: Vec<ksni::MenuItem<Self>> = vec![
                StandardItem {
                    label: self.labels.show.clone(),
                    activate: Box::new(|t: &mut Self| {
                        let _ = t.tx.send(Cmd::Show);
                    }),
                    ..Default::default()
                }
                .into(),
                ksni::MenuItem::Separator,
            ];
            // Read when the menu opens, so new profiles show up.
            for p in ProfileStore::user().list().0 {
                let label = p.name.clone();
                items.push(
                    StandardItem {
                        label,
                        activate: Box::new(move |_: &mut Self| apply(p.clone())),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            items.push(
                StandardItem {
                    label: self.labels.reset.clone(),
                    activate: Box::new(|_: &mut Self| {
                        std::thread::spawn(|| {
                            let _ = ipc::call_done(&Request::Reset { gpu: "0".into() });
                        });
                    }),
                    ..Default::default()
                }
                .into(),
            );
            items.push(ksni::MenuItem::Separator);
            items.push(
                StandardItem {
                    label: self.labels.quit.clone(),
                    activate: Box::new(|t: &mut Self| {
                        let _ = t.tx.send(Cmd::Quit);
                    }),
                    ..Default::default()
                }
                .into(),
            );
            items
        }
    }

    // StatusNotifierItem wants ARGB32 in network byte order.
    let icon = icon_rgba()
        .map(|(rgba, w, h)| {
            let data = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[3], p[0], p[1], p[2]])
                .collect();
            vec![ksni::Icon {
                width: w as i32,
                height: h as i32,
                data,
            }]
        })
        .unwrap_or_default();
    std::thread::Builder::new()
        .name("wnvctl-tray".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                // No tray host (e.g. a bare compositor): keep running without one.
                if let Ok(_handle) = (Tray { tx, labels, icon }).spawn().await {
                    std::future::pending::<()>().await;
                }
            });
        })
        .ok();
}

// ---- links and autostart ------------------------------------------------------

/// Opens a folder in the file manager or a URL in the browser.
pub fn open_external(target: &str) {
    let _ = std::process::Command::new("xdg-open").arg(target).spawn();
}

/// Enables or disables starting the GUI (hidden, in the tray) at login.
pub fn set_autostart(enabled: bool) -> std::io::Result<()> {
    let dir = dirs::config_dir().unwrap_or_default().join("autostart");
    let file = dir.join("wnvctl.desktop");
    if !enabled {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe()?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        &file,
        format!(
            "[Desktop Entry]\nType=Application\nName=wnvctl\n\
             Exec=\"{}\" --hidden\nIcon=wnvctl\nX-GNOME-Autostart-enabled=true\n\
             NoDisplay=true\n",
            exe.display()
        ),
    )
}
