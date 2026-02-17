use self_update::cargo_crate_version;
use native_dialog::{MessageDialog, MessageType};

pub struct Updater;

impl Updater {
    pub async fn check_and_update(silent: bool) -> anyhow::Result<()> {
        let current_ver = cargo_crate_version!();

        // Используем ReleaseList::fetch(), так как он возвращает пустой массив [],
        // если релизов нет, а не ошибку 403/404.
        let releases = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<self_update::update::Release>> {
            let rels = self_update::backends::github::ReleaseList::configure()
                .repo_owner("Kovalssky")
                .repo_name("bluetooth_audio_receiver")
                .build()
                .map_err(|e| anyhow::anyhow!("Ошибка конфигурации: {}", e))?
                .fetch()
                .map_err(|e| anyhow::anyhow!("Ошибка запроса к GitHub (возможно, лимит запросов): {}", e))?;
            Ok(rels)
        }).await.map_err(|e| anyhow::anyhow!("Ошибка потока: {}", e))??;

        // --- ОБРАБОТКА ОТСУТСТВИЯ РЕЛИЗОВ ---
        if releases.is_empty() {
            if !silent {
                Self::show_info("Обновления", "На GitHub пока нет доступных выпусков (релизов).");
            }
            return Ok(()); // Просто выходим без ошибки
        }

        // Если релизы есть, берем самый свежий (первый в списке)
        let latest = &releases[0];

        // Сравниваем версии (v0.1.0 > 0.1.0)
        let is_greater = self_update::version::bump_is_greater(current_ver, &latest.version)
            .unwrap_or(false);

        if is_greater {
            let confirmed = MessageDialog::new()
                .set_type(MessageType::Info)
                .set_title("🆙 Доступно обновление")
                .set_text(&format!(
                    "Найдена новая версия: v{}\nВаша версия: v{}\n\nЖелаете обновить программу?",
                    latest.version, current_ver
                ))
                .show_confirm()
                .unwrap_or(false);

            if confirmed {
                Self::perform_update().await?;
            }
        } else if !silent {
            Self::show_info("✅ Обновлений нет", "У вас установлена самая последняя версия.");
        }

        Ok(())
    }

    async fn perform_update() -> anyhow::Result<()> {
        tokio::task::spawn_blocking(|| -> anyhow::Result<()> {
            self_update::backends::github::Update::configure()
                .repo_owner("Kovalssky")
                .repo_name("bluetooth_audio_receiver")
                .bin_name("BT-Audio-Receiver")
                .show_download_progress(true)
                .current_version(cargo_crate_version!())
                .build()
                .map_err(|e| anyhow::anyhow!("Ошибка сборки апдейтера: {}", e))?
                .update()
                .map_err(|e| anyhow::anyhow!("Ошибка при замене файла: {}", e))?;
            Ok(())
        }).await.map_err(|e| anyhow::anyhow!("Критическая ошибка потока: {}", e))??;

        Ok(())
    }

    fn show_info(title: &str, text: &str) {
        let _ = MessageDialog::new()
            .set_type(MessageType::Info)
            .set_title(title)
            .set_text(text)
            .show_alert();
    }
}