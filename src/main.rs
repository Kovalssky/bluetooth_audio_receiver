#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bluetooth_receiver;
mod utils;
mod updater;

use crate::utils::ensure_registry_settings;
use crate::bluetooth_receiver::{BTReceiver, BTDevice};
use crate::updater::Updater;

use anyhow::Result;
use std::process::exit;
use tokio::sync::mpsc;

use winreg::enums::*;
use winreg::RegKey;

use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, CheckMenuItem},
    TrayIconBuilder, TrayIcon,
};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::windows::EventLoopBuilderExtWindows;

enum AppCommand {
    Connect(String),
    Disconnect,
    Scan,
    Reconnect(String),
}

// Структура приложения для управления состоянием в цикле событий
struct BTApp {
    tray: TrayIcon,
    menu_event_receiver: tray_icon::menu::MenuEventReceiver,
    rx_devices: mpsc::Receiver<Vec<BTDevice>>,
    rx_conn_status: mpsc::Receiver<Option<String>>,
    cmd_tx: mpsc::Sender<AppCommand>,
    current_devices: Vec<BTDevice>,
    current_connected: Option<String>,
}

impl ApplicationHandler for BTApp {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _window_id: winit::window::WindowId, _event: WindowEvent) {}

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let mut changed = false;

        // 1. Обработка нажатий в меню трея
        while let Ok(event) = self.menu_event_receiver.try_recv() {
            match event.id.as_ref() {
                "quit_app" => exit(0),
                "check_update" => {
                    tokio::spawn(async {
                        if let Err(e) = Updater::check_and_update(false).await {
                            eprintln!("{}", e);
                            show_error_dialog("Ошибка обновления", &e.to_string());
                        }
                    });
                }
                "refresh" => {
                    if let Err(e) = self.cmd_tx.try_send(AppCommand::Scan) {
                        eprintln!("[UI] Ошибка отправки команды: {}", e);
                    }
                }
                "disconnect" => {
                    if let Err(e) = self.cmd_tx.try_send(AppCommand::Disconnect) {
                        eprintln!("[UI] Ошибка отправки команды: {}", e);
                    }
                }
                "toggle_autostart" => {
                    let current = is_autostart_enabled();
                    let _ = set_autostart(!current);
                    changed = true;
                }
                id if id.starts_with("dev:") => {
                    let name = id[4..].to_string();
                    if let Err(e) = self.cmd_tx.try_send(AppCommand::Connect(name)) {
                        eprintln!("[UI] Ошибка отправки команды: {}", e);
                    }
                }
                id if id.starts_with("reconnect:") => {
                    let name = id[10..].to_string();
                    if let Err(e) = self.cmd_tx.try_send(AppCommand::Reconnect(name)) {
                        eprintln!("[UI] Ошибка отправки команды: {}", e);
                    }
                }
                _ => {}
            }
        }

        // 2. Получение новых списков устройств из Bluetooth воркера
        while let Ok(devices) = self.rx_devices.try_recv() {
            self.current_devices = devices;
            changed = true;
        }

        // 3. Получение статусов подключения
        while let Ok(status) = self.rx_conn_status.try_recv() {
            self.current_connected = status;
            changed = true;
        }

        // 4. Если что-то изменилось — перерисовываем меню
        if changed {
            let new_menu = build_menu(&self.current_devices, self.current_connected.clone());
            let _ = self.tray.set_menu(Some(Box::new(new_menu)));
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let (tx_devices, rx_devices) = mpsc::channel::<Vec<BTDevice>>(10);
    let (tx_conn_status, rx_conn_status) = mpsc::channel::<Option<String>>(10);
    let (cmd_tx, cmd_rx) = mpsc::channel::<AppCommand>(10);

    // Применяем настройки реестра
    ensure_registry_settings().expect("Failed to fix registry");

    // Создаем трей
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(build_menu(&[], None)))
        .with_tooltip("BT Audio Receiver")
        .with_icon(load_icon())
        .build()?;

    // Клоны для фонового потока
    let tx_dev_bg = tx_devices.clone();
    let tx_stat_bg = tx_conn_status.clone();

    // Запуск воркера Bluetooth
    tokio::spawn(async move {
        let mut receiver = BTReceiver::new();
        let _ = background_worker(&mut receiver, tx_dev_bg, tx_stat_bg, cmd_rx).await;
    });

    // Настройка EventLoop
    let event_loop = EventLoop::builder().with_any_thread(true).build()?;
    event_loop.set_control_flow(ControlFlow::WaitUntil(
        std::time::Instant::now() + std::time::Duration::from_millis(50)
    ));

    let mut app = BTApp {
        tray,
        menu_event_receiver: MenuEvent::receiver().clone(),
        rx_devices,
        rx_conn_status,
        cmd_tx: cmd_tx.clone(),
        current_devices: Vec::new(),
        current_connected: None,
    };

    event_loop.run_app(&mut app)?;

    Ok(())
}

async fn background_worker(
    receiver: &mut BTReceiver,
    tx_dev: mpsc::Sender<Vec<BTDevice>>,
    tx_stat: mpsc::Sender<Option<String>>,
    mut cmd_rx: mpsc::Receiver<AppCommand>,
) -> Result<()> {
    // Начальное сканирование
    if let Ok(devs) = receiver.list_devices().await {
        let _ = tx_dev.send(devs).await;
    }

    loop {
        if let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                AppCommand::Scan => {
                    if let Ok(devs) = receiver.list_devices().await {
                        let _ = tx_dev.send(devs).await;
                    }
                }
                AppCommand::Connect(name) => {
                    let devs = receiver.list_devices().await.unwrap_or_default();
                    if let Some(target) = devs.iter().find(|d| d.name == name) {
                        if receiver.connect(target).await.is_ok() {
                            let _ = tx_stat.send(Some(name)).await;
                        }
                    }
                }
                AppCommand::Disconnect => {
                    receiver.disconnect().await;
                    let _ = tx_stat.send(None).await;
                }
                AppCommand::Reconnect(name) => {
                    let devs = receiver.list_devices().await.unwrap_or_default();
                    if let Some(target) = devs.iter().find(|d| d.name == name) {
                        if receiver.reconnect(target).await.is_ok() {
                            let _ = tx_stat.send(Some(name)).await;
                        }
                    }
                }
            }
        }
    }
}

fn show_error_dialog(title: &str, message: &str) {
    use native_dialog::{MessageDialog, MessageType};
    MessageDialog::new()
        .set_type(MessageType::Error)
        .set_title(title)
        .set_text(&format!("{}\n\nПроверьте подключение к интернету.", message))
        .show_alert()
        .unwrap();
}

fn set_autostart(enable: bool) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let path = r"Software\Microsoft\Windows\CurrentVersion\Run";
    let (key, _) = hkcu.create_subkey(path)?;

    if enable {
        let current_exe = std::env::current_exe()?;
        key.set_value("BTAudioReceiver", &current_exe.to_str().unwrap_or(""))?;
    } else {
        let _ = key.delete_value("BTAudioReceiver");
    }
    Ok(())
}

fn is_autostart_enabled() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let path = r"Software\Microsoft\Windows\CurrentVersion\Run";
    if let Ok(key) = hkcu.open_subkey(path) {
        let val: String = key.get_value("BTAudioReceiver").unwrap_or_default();
        return !val.is_empty();
    }
    false
}

fn build_menu(devices: &[BTDevice], connected_to: Option<String>) -> Menu {
    let menu = Menu::new();

    if let Some(ref name) = connected_to {
        let _ = menu.append(&MenuItem::with_id("status", &format!("✅ {}", name), false, None));
        let _ = menu.append(&MenuItem::with_id(format!("reconnect:{}", name), "🔄 Переподключить", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("disconnect", "🔌 Отключить", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
    }

    if devices.is_empty() {
        let _ = menu.append(&MenuItem::with_id("none", "(Нет устройств)", false, None));
    } else {
        for device in devices {
            if connected_to.as_ref() != Some(&device.name) {
                let id = format!("dev:{}", device.name);
                let _ = menu.append(&MenuItem::with_id(id, &format!("📱 {}", device.name), true, None));
            }
        }
    }

    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id("refresh", "🔄 Обновить список", true, None));
    let _ = menu.append(&MenuItem::with_id("check_update", "🆙 Проверить обновления", true, None));

    let autostart_item = CheckMenuItem::with_id(
        "toggle_autostart",
        "Автозагрузка",
        true,
        is_autostart_enabled(),
        None
    );
    let _ = menu.append(&autostart_item);

    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id("quit_app", "❌ Выйти", true, None));

    menu
}

fn load_icon() -> tray_icon::Icon {
    let bytes = include_bytes!("icon.ico");
    let image = image::load_from_memory(bytes).expect("icon.ico error").into_rgba8();
    let (w, h) = image.dimensions();
    tray_icon::Icon::from_rgba(image.into_raw(), w, h).unwrap()
}