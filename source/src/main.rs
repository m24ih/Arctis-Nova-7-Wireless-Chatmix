use anyhow::{Context, Result};
use ctrlc;
use env_logger;
use hidapi::HidApi;
use log::{debug, error, info, warn};
use rusb::{DeviceHandle, UsbContext};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use std::env;

const VENDOR_ID: u16 = 0x1038;
const PRODUCT_ID: u16 = 0x2202;
const HID_MSG_SIZE: usize = 64;

struct ArctisController {
    original_default_sink: String,
    running: Arc<AtomicBool>,
    sinks_created: Arc<AtomicBool>,
}

impl ArctisController {
    fn new() -> Result<Self> {
        let original_default_sink = get_default_sink()?;
        info!("Original default sink: {}", original_default_sink);

        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();

        ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
        })
        .context("Failed to set Ctrl+C handler")?;

        Ok(Self {
            original_default_sink,
            running,
            sinks_created: Arc::new(AtomicBool::new(false)),
        })
    }

    fn setup_virtual_sinks(&self) -> Result<()> {
        if self.sinks_created.load(Ordering::SeqCst) {
            info!("Virtual sinks already exist, skipping creation");
            return Ok(());
        }

        let arctis_sink = find_arctis_sink().context("Arctis 7 device not found in audio system")?;
        info!("Arctis sink: {}", arctis_sink);

        info!("Cleaning up old virtual sinks...");
        let _ = Command::new("pw-cli")
            .args(&["destroy", "Arctis_Game"])
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("pw-cli")
            .args(&["destroy", "Arctis_Chat"])
            .stderr(Stdio::null())
            .status();

        std::thread::sleep(Duration::from_millis(500));

        info!("Creating virtual sinks...");
        let game_result = Command::new("pw-cli")
            .args(&[
                "create-node",
                "adapter",
                r#"{factory.name=support.null-audio-sink node.name=Arctis_Game node.description="Arctis 7+ Game" media.class=Audio/Sink monitor.channel-volumes=true object.linger=true audio.position=[FL FR]}"#
            ])
            .stdout(Stdio::null())
            .status()
            .context("Failed to create Game sink")?;

        if !game_result.success() {
            anyhow::bail!("Failed to create Arctis_Game sink");
        }

        let chat_result = Command::new("pw-cli")
            .args(&[
                "create-node",
                "adapter",
                r#"{factory.name=support.null-audio-sink node.name=Arctis_Chat node.description="Arctis 7+ Chat" media.class=Audio/Sink monitor.channel-volumes=true object.linger=true audio.position=[FL FR]}"#
            ])
            .stdout(Stdio::null())
            .status()
            .context("Failed to create Chat sink")?;

        if !chat_result.success() {
            anyhow::bail!("Failed to create Arctis_Chat sink");
        }

        std::thread::sleep(Duration::from_millis(1000));

        info!("Linking virtual sinks to headset...");
        link_sink_to_device("Arctis_Game", &arctis_sink)?;
        link_sink_to_device("Arctis_Chat", &arctis_sink)?;

        info!("Setting Arctis_Game as default sink...");
        Command::new("pactl")
            .args(&["set-default-sink", "Arctis_Game"])
            .status()
            .context("Failed to set default sink")?;

        self.sinks_created.store(true, Ordering::SeqCst);
        info!("Setup complete!");

        Ok(())
    }

    fn start(&self) -> Result<()> {
        loop {
            if !self.running.load(Ordering::SeqCst) {
                return Ok(());
            }

            info!("Waiting for Arctis 7 device...");
            match self.setup_virtual_sinks() {
                Ok(_) => break,
                Err(e) => {
                    warn!("Setup failed: {}. Retrying in 3 seconds...", e);
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
        }

        loop {
            if !self.running.load(Ordering::SeqCst) {
                break;
            }

            match self.try_connect_and_run() {
                Ok(_) => {
                    info!("Connection loop ended gracefully; exiting main loop");
                    break;
                }
                Err(e) => {
                    if !self.running.load(Ordering::SeqCst) {
                        break;
                    }

                    warn!("USB connection lost / error: {}", e);
                    info!("Device disconnected or became unresponsive. Waiting for reconnection...");
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
        }

        Ok(())
    }

    // Keep trying to open the USB device while running is true.
    // When opened, run the read loop; propagate Err on unexpected disconnect so the caller retries.
    fn try_connect_and_run(&self) -> Result<()> {
        let usb_ctx = rusb::Context::new().context("Failed to initialize libusb context")?;

        while self.running.load(Ordering::SeqCst) {
            match usb_find_and_open(&usb_ctx) {
                Ok((mut handle, endpoint, interface_num)) => {
                    info!("{}", "=".repeat(50));
                    info!("Arctis 7+ ChatMix Enabled!");
                    info!("  • Arctis_Game - for game audio");
                    info!("  • Arctis_Chat - for chat/voice audio");
                    info!("{}", "=".repeat(50));

                    // Re-link virtual sinks to the freshly-attached physical device.
                    if let Err(e) = self.relink_virtual_sinks_with_retry() {
                        warn!("Failed to relink virtual sinks after reconnect: {}", e);
                    }

                    // Ensure all current streams are moved to Arctis_Game
                    if let Err(e) = move_all_inputs_to("Arctis_Game") {
                        warn!("Failed to move existing sink inputs to Arctis_Game: {}", e);
                    } else {
                        info!("Moved existing sink-inputs to Arctis_Game");
                    }

                    // Run the read loop. If it returns Err, propagate to allow reconnection attempts.
                    let res = self.read_loop(&mut handle, endpoint);

                    // Try releasing the interface; ignore errors (device may already be gone).
                    if let Err(e) = handle.release_interface(interface_num) {
                        warn!("Failed to release interface (device may be gone): {:?}", e);
                    }

                    return res;
                }
                Err(e) => {
                    if !self.running.load(Ordering::SeqCst) {
                        break;
                    }
                    debug!("usb_find_and_open failed: {:?}", e);
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }

        Ok(())
    }

    // Read HID reports and apply volumes. On repeated non-timeout errors or NoDevice, return Err.
    fn read_loop<T: UsbContext>(&self, handle: &mut DeviceHandle<T>, endpoint: u8) -> Result<()> {
        let mut buf = [0u8; 64];
        let mut consecutive_errors = 0u32;
        const MAX_ERRORS: u32 = 5;

        while self.running.load(Ordering::SeqCst) {
            match handle.read_interrupt(endpoint, &mut buf, Duration::from_millis(1000)) {
                Ok(len) => {
                    consecutive_errors = 0; // reset the error counter on success

                    if len >= 3 && buf[0] == 0x45 {
                        let game_vol = buf[1];
                        let chat_vol = buf[2];

                        set_sink_volume("Arctis_Game", game_vol);
                        set_sink_volume("Arctis_Chat", chat_vol);
                    }
                }
                Err(rusb::Error::Timeout) => {
                    consecutive_errors = 0;
                    continue;
                }
                Err(rusb::Error::NoDevice) => {
                    error!("Device disconnected (NoDevice)");
                    return Err(anyhow::anyhow!("USB device disconnected (NoDevice)"));
                }
                Err(rusb::Error::Io) => {
                    consecutive_errors += 1;
                    warn!("USB I/O error (attempt {}/{})", consecutive_errors, MAX_ERRORS);
                    if consecutive_errors >= MAX_ERRORS {
                        error!("Too many I/O errors - device likely disconnected");
                        return Err(anyhow::anyhow!("Too many USB I/O errors"));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    consecutive_errors += 1;
                    warn!("USB error: {:?} (attempt {}/{})", e, consecutive_errors, MAX_ERRORS);
                    if consecutive_errors >= MAX_ERRORS {
                        error!("Too many USB errors - giving up: {:?}", e);
                        return Err(anyhow::anyhow!("Too many USB errors: {:?}", e));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }

        info!("USB read loop ended due to program shutdown (Ctrl-C or running=false)");
        Ok(())
    }

    // Try multiple times to find the physical headset's sink and re-link the virtual sinks to it.
    fn relink_virtual_sinks_with_retry(&self) -> Result<()> {
        const RETRIES: usize = 10;
        for attempt in 1..=RETRIES {
            if !self.running.load(Ordering::SeqCst) {
                anyhow::bail!("Shutdown in progress");
            }

            match find_arctis_sink() {
                Ok(arctis_sink) => {
                    info!("Relinking virtual sinks to device '{}'", arctis_sink);

                    if let Err(e) = link_sink_to_device("Arctis_Game", &arctis_sink) {
                        warn!("Failed to link Arctis_Game -> {}: {}", arctis_sink, e);
                    } else {
                        info!("Linked Arctis_Game -> {}", arctis_sink);
                    }

                    if let Err(e) = link_sink_to_device("Arctis_Chat", &arctis_sink) {
                        warn!("Failed to link Arctis_Chat -> {}: {}", arctis_sink, e);
                    } else {
                        info!("Linked Arctis_Chat -> {}", arctis_sink);
                    }

                    let status = Command::new("pactl")
                        .args(&["set-default-sink", "Arctis_Game"])
                        .status();

                    match status {
                        Ok(s) if s.success() => info!("Set Arctis_Game as default sink"),
                        Ok(s) => warn!("pactl set-default-sink returned non-zero: {:?}", s),
                        Err(e) => warn!("Failed to run pactl: {:?}", e),
                    }

                    std::thread::sleep(Duration::from_millis(300));
                    return Ok(());
                }
                Err(e) => {
                    debug!("Could not find Arctis sink on attempt {}/{}: {}", attempt, RETRIES, e);
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
            }
        }

        anyhow::bail!("Failed to locate Arctis sink after retries");
    }

    fn cleanup(&self) {
        info!("Shutting down...");

        let _ = Command::new("pactl")
            .args(&["set-default-sink", &self.original_default_sink])
            .status();

        info!("Destroying virtual sinks...");
        let _ = Command::new("pw-cli")
            .args(&["destroy", "Arctis_Game"])
            .stdout(Stdio::null())
            .status();
        let _ = Command::new("pw-cli")
            .args(&["destroy", "Arctis_Chat"])
            .stdout(Stdio::null())
            .status();

        info!("Arctis 7+ ChatMix shut down gracefully.");
    }
}

impl Drop for ArctisController {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn main() -> Result<()> {
    env_logger::init();

    info!("Initializing Arctis 7+ ChatMix...");

    let controller = ArctisController::new()?;
    controller.start()?;

    Ok(())
}

fn get_default_sink() -> Result<String> {
    let output = Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .context("Failed to get default sink")?;

    let sink = String::from_utf8(output.stdout)?
        .trim()
        .to_string();

    if sink.is_empty() {
        anyhow::bail!("Could not determine default sink");
    }

    Ok(sink)
}

fn find_arctis_sink() -> Result<String> {
    let output = Command::new("pactl")
        .args(&["list", "short", "sinks"])
        .output()
        .context("Failed to list sinks")?;

    let sinks = String::from_utf8(output.stdout)?;

    let mut fallback: Option<String> = None;
    for line in sinks.lines() {
        let lower = line.to_lowercase();
        if lower.contains("arctis") && lower.contains("7") {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 2 {
                let name = parts[1].to_string();
                if lower.contains("usb") || lower.contains("playback") || lower.contains("dot") || lower.contains("wireless") {
                    return Ok(name);
                }
                if fallback.is_none() {
                    fallback = Some(name);
                }
            }
        }
    }

    if let Some(f) = fallback {
        return Ok(f);
    }

    anyhow::bail!("No Arctis 7 device found in pactl output");
}

fn link_sink_to_device(sink_name: &str, device_name: &str) -> Result<()> {
    for channel in ["FL", "FR"] {
        let src = format!("{}:monitor_{}", sink_name, channel);
        let dst = format!("{}:playback_{}", device_name, channel);

        let output = Command::new("pw-link")
            .arg(&src)
            .arg(&dst)
            .output()
            .context(format!("Failed to execute pw-link for {}", channel))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("File exists") || stderr.contains("exists") {
                warn!("Link already exists (skipping): {} -> {}", src, dst);
            } else {
                anyhow::bail!("pw-link failed for {}: {}", channel, stderr.trim());
            }
        }
    }

    Ok(())
}

fn set_sink_volume(sink_name: &str, volume_percent: u8) {
    let result = Command::new("pactl")
        .args(&["set-sink-volume", sink_name, &format!("{}%", volume_percent)])
        .output();

    if let Ok(output) = result {
        if !output.status.success() {
            warn!(
                "Failed to set volume for {}: {}",
                sink_name,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

fn move_all_inputs_to(sink_name: &str) -> Result<()> {
    let output = Command::new("pactl")
        .args(&["list", "short", "sink-inputs"])
        .output()
        .context("Failed to list sink-inputs")?;

    let stdout = String::from_utf8(output.stdout)?;
    for line in stdout.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if !cols.is_empty() {
            if let Ok(index) = cols[0].parse::<u32>() {
                let status = Command::new("pactl")
                    .args(&["move-sink-input", &index.to_string(), sink_name])
                    .status()
                    .context(format!("Failed to move sink-input {}", index))?;
                if !status.success() {
                    warn!("pactl move-sink-input {} -> {} returned {:?}", index, sink_name, status);
                } else {
                    debug!("Moved sink-input {} -> {}", index, sink_name);
                }
            }
        }
    }

    Ok(())
}

/* ---------- hidapi sidetone write (uses the real HidApi::open + write) ---------- */

/// Try to send sidetone report via hidapi if environment requests it.
/// This uses HidApi::open() and device.write() and mirrors your provided snippet.
/// Non-fatal: errors are logged but do not stop device claim.
fn try_hidapi_sidetone_from_env() {
    // Determine desired percent or disable flag
    if let Ok(v) = env::var("ARCTIS_SIDETONE_DISABLE") {
        if matches!(v.as_str(), "1" | "yes" | "true" | "on") {
            if let Err(e) = hidapi_send_sidetone(0) {
                warn!("hidapi sidetone write failed: {}", e);
            }
            return;
        }
        else {
            if let Err(e) = hidapi_send_sidetone(100) {
                warn!("hidapi sidetone write failed: {}", e);
            }
            return;
        }
    }

    if let Ok(v) = env::var("ARCTIS_SIDETONE_PERCENT") {
        if let Ok(mut num) = v.trim().parse::<u8>() {
            if num > 100 {
                num = 100;
            }
            if let Err(e) = hidapi_send_sidetone(num) {
                warn!("hidapi sidetone write failed: {}", e);
            }
        } else {
            debug!("ARCTIS_SIDETONE_PERCENT invalid: '{}'", v);
        }
    }
}

/// Build 64-byte buffer and write using HidApi
fn hidapi_send_sidetone(percent: u8) -> Result<()> {
    // bucket mapping per your snippet: <30->0, <60->1, <80->2, else 3
    let bucket = if percent < 30 {
        0x00u8
    } else if percent < 60 {
        0x01u8
    } else if percent < 80 {
        0x02u8
    } else {
        0x03u8
    };

    let mut data = [0u8; HID_MSG_SIZE];
    data[0] = 0x00;
    data[1] = 0x39;
    data[2] = bucket;

    match HidApi::new() {
        Ok(api) => match api.open(VENDOR_ID, PRODUCT_ID) {
            Ok(device) => match device.write(&data) {
                Ok(n) => {
                    info!("hidapi: wrote {} bytes for sidetone (bucket {})", n, bucket);
                }
                Err(e) => {
                    anyhow::bail!("hidapi write error: {}", e);
                }
            },
            Err(e) => {
                anyhow::bail!("hidapi open error: {}", e);
            }
        },
        Err(e) => {
            anyhow::bail!("hidapi init error: {}", e);
        }
    }

    Ok(())
}

/* ---------- end hidapi sidetone ---------- */

// Return (handle, endpoint_addr, interface_number)
// This version tries to enable libusb auto-detach, falls back to manual detach,
// and retries claiming the interface a few times to handle the kernel re-attaching quickly.
fn usb_find_and_open<T: UsbContext>(usb_ctx: &T) -> Result<(DeviceHandle<T>, u8, u8)> {
    let dev = usb_ctx
        .devices()?
        .iter()
        .find(|d| {
            if let Ok(desc) = d.device_descriptor() {
                desc.vendor_id() == VENDOR_ID && desc.product_id() == PRODUCT_ID
            } else {
                false
            }
        })
        .ok_or_else(|| anyhow::anyhow!("Arctis Nova 7 not found. Please ensure it is connected."))?;

    info!("Found Arctis Nova 7 device");

    let config = dev.config_descriptor(0)?;
    let mut target_interface_num = None;
    let mut target_endpoint = 0x84u8;

    for interface in config.interfaces() {
        if let Some(desc) = interface.descriptors().next() {
            if desc.class_code() == 3 {
                for endpoint in desc.endpoint_descriptors() {
                    if endpoint.transfer_type() == rusb::TransferType::Interrupt
                        && endpoint.direction() == rusb::Direction::In
                    {
                        target_interface_num = Some(desc.interface_number());
                        target_endpoint = endpoint.address();
                        info!(
                            "Using interface {}, endpoint 0x{:02x}",
                            desc.interface_number(),
                            endpoint.address()
                        );
                        break;
                    }
                }
            }
        }
    }

    let interface_num = target_interface_num
        .ok_or_else(|| anyhow::anyhow!("Could not find HID interface"))?;

    // Open the device with rusb (we will still use hidapi for the write above)
    let mut handle = dev.open().context("Failed to open USB device")?;

    // Attempt an hidapi sidetone write before claiming the interface.
    // (your working HidApi approach opens the device via hidraw and writes 64 bytes)
    try_hidapi_sidetone_from_env();

    // Try to enable libusb automatic kernel-driver detaching (if available).
    match handle.set_auto_detach_kernel_driver(true) {
        Ok(_) => info!("Enabled auto-detach kernel driver on handle"),
        Err(e) => {
            debug!("Could not enable auto-detach kernel driver: {:?}", e);
        }
    }

    const CLAIM_RETRIES: usize = 6;
    for attempt in 1..=CLAIM_RETRIES {
        match handle.kernel_driver_active(interface_num) {
            Ok(true) => {
                match handle.detach_kernel_driver(interface_num) {
                    Ok(_) => info!("Detached kernel driver from interface {}", interface_num),
                    Err(e) => warn!(
                        "Failed to detach kernel driver (attempt {}): {:?}",
                        attempt, e
                    ),
                }
            }
            Ok(false) => { }
            Err(e) => {
                debug!("kernel_driver_active check failed: {:?}", e);
            }
        }

        match handle.claim_interface(interface_num) {
            Ok(_) => {
                info!("Successfully claimed interface {}", interface_num);
                return Ok((handle, target_endpoint, interface_num));
            }
            Err(e) => {
                warn!(
                    "claim_interface failed on attempt {}/{}: {:?}",
                    attempt, CLAIM_RETRIES, e
                );
                if attempt == CLAIM_RETRIES {
                    return Err(anyhow::anyhow!(
                        "Failed to claim interface {} after {} attempts: {:?}",
                        interface_num,
                        CLAIM_RETRIES,
                        e
                    ));
                }
                std::thread::sleep(Duration::from_millis(200 * attempt as u64));
                continue;
            }
        }
    }

    Err(anyhow::anyhow!("Failed to open and claim device"))
}
