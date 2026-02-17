use anyhow::{Result, Context};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::{w, HSTRING};
use windows::Devices::Enumeration::DeviceInformation;
use windows::Media::Audio::*;
use windows::Media::Render::AudioRenderCategory;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;

#[derive(Clone)]
pub struct BTDevice {
    pub name: String,
    pub info: DeviceInformation,
}

pub struct BTReceiver {
    pub connection: Option<AudioPlaybackConnection>,
    pub graph: Option<AudioGraph>,
    // Используем AtomicBool для мгновенного и безопасного управления потоком мониторинга
    is_monitoring: Arc<AtomicBool>,
    pub device_id: Option<HSTRING>,
    avrt_handle: Option<HANDLE>,
}

impl BTReceiver {
    pub fn new() -> Self {
        Self {
            connection: None,
            graph: None,
            is_monitoring: Arc::new(AtomicBool::new(false)),
            device_id: None,
            avrt_handle: None,
        }
    }

    pub async fn connect(&mut self, device: &BTDevice) -> Result<()> {
        let id = device.info.Id()?;
        self.device_id = Some(id.clone());
        println!("[INIT] Подключение к {}...", device.name);
        self.perform_connect().await
    }

    pub async fn reconnect(&mut self, device: &BTDevice) -> Result<()> {
        println!("[INIT] Переподключение к {}...", device.name);
        self.disconnect().await;
        // Заменяем expect на оператор ?, чтобы не «ронять» приложение при ошибке
        self.perform_connect().await.context("Ошибка переподключения")?;
        Ok(())
    }

    async fn perform_connect(&mut self) -> Result<()> {
        let id = self.device_id.as_ref().ok_or_else(|| anyhow::anyhow!("ID не найден"))?;
        let conn = AudioPlaybackConnection::TryCreateFromId(id)?;
        conn.Start()?;

        let result = conn.OpenAsync()?.await?;
        if result.Status()? != AudioPlaybackConnectionOpenResultStatus::Success {
            anyhow::bail!("Статус открытия: {:?}", result.Status()?);
        }

        println!("[CONN] Соединение активно");
        self.connection = Some(conn);

        // 1. Удержание канала через AudioGraph (Prevent Sleep)
        self.prevent_sleep_with_anchor().await?;

        // 2. Мониторинг (Heartbeat)
        self.start_heartbeat_monitor();

        // 3. MMCSS (Multi-Media Class Scheduler Service)
        unsafe {
            let mut task_index = 0u32;
            if let Ok(handle) = AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) {
                self.avrt_handle = Some(handle);
            }
        }

        Ok(())
    }

    fn start_heartbeat_monitor(&self) {
        // Останавливаем предыдущий монитор, если он был
        self.is_monitoring.store(false, Ordering::SeqCst);

        let is_monitoring = self.is_monitoring.clone();
        let device_id = self.device_id.clone();

        is_monitoring.store(true, Ordering::SeqCst);

        tokio::spawn(async move {
            let mut tick = 0u64;
            println!("[MONITOR] Heartbeat запущен.");

            while is_monitoring.load(Ordering::SeqCst) {
                tick += 1;

                if let Some(ref id) = device_id {
                    if let Ok(conn) = AudioPlaybackConnection::TryCreateFromId(id) {
                        let _ = conn.Start();
                        // Раз в 30 секунд делаем более "тяжелый" пинг для сброса таймеров Windows
                        if tick % 15 == 0 {
                            let _ = conn.OpenAsync();
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            println!("[MONITOR] Heartbeat остановлен.");
        });
    }

    async fn prevent_sleep_with_anchor(&mut self) -> Result<()> {
        let settings = AudioGraphSettings::Create(AudioRenderCategory::Media)?;
        // Устанавливаем квант времени для уменьшения нагрузки
        settings.SetQuantumSizeSelectionMode(QuantumSizeSelectionMode::LowestLatency)?;

        let create_result = AudioGraph::CreateAsync(&settings)?.await?;
        let graph = create_result.Graph()?;

        let output_result = graph.CreateDeviceOutputNodeAsync()?.await?;
        if output_result.Status()? == AudioDeviceNodeCreationStatus::Success {
            let output_node = output_result.DeviceOutputNode()?;

            // Создаем тишину
            let frame_input = graph.CreateFrameInputNode()?;
            frame_input.AddOutgoingConnection(&output_node)?;

            // Уровень усиления 0.0001 достаточен, чтобы Windows считала поток активным,
            // но пользователь ничего не слышал.
            output_node.SetOutgoingGain(0.0001)?;
        }

        graph.Start()?;
        self.graph = Some(graph);
        Ok(())
    }

    pub async fn list_devices(&self) -> Result<Vec<BTDevice>> {
        let selector = AudioPlaybackConnection::GetDeviceSelector()?;
        let devices = DeviceInformation::FindAllAsyncAqsFilter(&selector)?.await?;

        let mut result = Vec::new();
        for info in devices {
            if let Ok(name) = info.Name() {
                result.push(BTDevice { name: name.to_string(), info });
            }
        }
        Ok(result)
    }

    pub async fn disconnect(&mut self) {
        // Сигнализируем монитору остановиться
        self.is_monitoring.store(false, Ordering::SeqCst);

        if let Some(graph) = self.graph.take() {
            let _ = graph.Stop();
            let _ = graph.Close(); // Явное закрытие ресурсов
        }

        self.connection = None;
        self.avrt_handle = None;

        println!("[DISCONN] Соединение закрыто.");
    }
}