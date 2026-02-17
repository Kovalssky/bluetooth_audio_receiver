use winreg::enums::*;
use winreg::RegKey;
use std::process::Command;
use std::env;
use anyhow::Result;

const SINK_PATH: &str = r"SYSTEM\CurrentControlSet\Control\Bluetooth\Audio\A2dp\Sink";
const BTHPORT_PATH: &str = r"SYSTEM\CurrentControlSet\Services\BTHPORT\Parameters";
const BTHA2DP_PATH: &str = r"SYSTEM\CurrentControlSet\Services\BthA2dp\Parameters";

pub fn ensure_registry_settings() -> Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    // Проверяем все важные ключи
    let mut needs_fix = false;

    // 1. Проверка Sink
    if let Ok(key) = hklm.open_subkey(SINK_PATH) {
        let snoop: u32 = key.get_value("DisableSnoop").unwrap_or(0);
        let offload: u32 = key.get_value("DisableOffload").unwrap_or(0);
        if snoop != 1 || offload != 1 { needs_fix = true; }
    } else { needs_fix = true; }

    // 2. Проверка BTHPORT
    if let Ok(key) = hklm.open_subkey(BTHPORT_PATH) {
        let wake: u32 = key.get_value("SystemRemoteWakeSupported").unwrap_or(0);
        if wake != 1 { needs_fix = true; }
    } else { needs_fix = true; }

    if needs_fix {
        let args: Vec<String> = env::args().collect();
        if args.contains(&"--fix-registry".to_string()) {
            run_registry_fix()?;
        } else {
            println!("[REG] Настройки неоптимальны. Запрашиваю права администратора...");
            elevate_self()?;
            std::process::exit(0);
        }
    } else {
        println!("[REG] Реестр в порядке (DisableSnoop=1, RemoteWake=1).");
    }

    Ok(())
}

fn run_registry_fix() -> Result<()> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

    // ВАЖНО: используем create_subkey_with_flags с KEY_ALL_ACCESS для записи

    // Исправляем A2dp Sink
    let (sink_key, _) = hklm.create_subkey_with_flags(SINK_PATH, KEY_ALL_ACCESS)?;
    sink_key.set_value("DisableSnoop", &1u32)?;
    sink_key.set_value("DisableOffload", &1u32)?;

    // Исправляем BTHPORT (отвечает за сон контроллера)
    let (bt_key, _) = hklm.create_subkey_with_flags(BTHPORT_PATH, KEY_ALL_ACCESS)?;
    bt_key.set_value("DisableSnoop", &1u32)?;
    bt_key.set_value("SystemRemoteWakeSupported", &1u32)?;

    // Исправляем BthA2dp (политика домена)
    let (bth_key, _) = hklm.create_subkey_with_flags(BTHA2DP_PATH, KEY_ALL_ACCESS)?;
    bth_key.set_value("DefaultDomainPolicy", &1u32)?;

    println!("[REG] Настройки успешно применены. Изменения вступят в силу после перезапуска Bluetooth.");
    Ok(())
}

fn elevate_self() -> Result<()> {
    let current_exe = env::current_exe()?;

    // Добавляем кавычки вокруг пути к exe на случай пробелов в имени папок
    let status = Command::new("powershell")
        .arg("-Command")
        .arg(format!(
            "Start-Process -FilePath '{}' -ArgumentList '--fix-registry' -Verb RunAs -Wait",
            current_exe.display()
        ))
        .status()?;

    if !status.success() {
        anyhow::bail!("Пользователь отклонил запрос UAC.");
    }
    Ok(())
}