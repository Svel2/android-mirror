use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidDevice {
    pub serial: String,
    pub status: String,
    pub model: String,
    pub brand: String,
    pub market_name: String,
    pub is_wifi: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceMetrics {
    pub cpu_pct: Option<u32>,
    pub gpu_pct: Option<u32>,
    pub battery_temp_c: Option<f32>,
    pub battery_pct: Option<u8>,
}

#[derive(Clone, Debug)]
pub struct MirrorOptions {
    pub serial: String,
    pub window_title: String,
    pub quality: usize,
    pub turn_screen_off: bool,
    pub stay_awake: bool,
    pub always_on_top: bool,
    pub forward_audio: bool,
    pub rounded_window: bool,
}

fn silent_cmd(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

pub fn local_bin_dir() -> PathBuf {
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("bin").join("scrcpy");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("bin")
        .join("scrcpy")
}

fn which(name: &str) -> Option<PathBuf> {
    let mut cmd = Command::new("where");
    cmd.arg(name);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
}

pub fn find_scrcpy() -> Option<PathBuf> {
    let local = local_bin_dir().join("scrcpy.exe");
    if local.is_file() {
        return Some(local);
    }
    which("scrcpy")
}

pub fn find_adb() -> Option<PathBuf> {
    let local = local_bin_dir().join("adb.exe");
    if local.is_file() {
        return Some(local);
    }
    let sdk_default = PathBuf::from(r"C:\Android\sdk\platform-tools\adb.exe");
    if sdk_default.is_file() {
        return Some(sdk_default);
    }
    which("adb")
}

fn guess_brand_from_model(model: &str) -> String {
    let m = model.to_ascii_uppercase();
    if m.starts_with("RMX") {
        "realme".to_string()
    } else if m.starts_with("SM-") {
        "samsung".to_string()
    } else if m.starts_with("PIXEL") {
        "google".to_string()
    } else if m.starts_with("CPH") {
        "oppo".to_string()
    } else if m.starts_with("V2") || m.starts_with("V1") {
        "vivo".to_string()
    } else if m.starts_with("2") && m.len() >= 10 {
        "xiaomi".to_string()
    } else {
        String::new()
    }
}

fn fetch_brand_and_market(adb: &Path, serial: &str) -> (String, String) {
    let out = silent_cmd(adb)
        .args([
            "-s",
            serial,
            "shell",
            "getprop ro.product.brand; echo '|'; getprop ro.product.marketname",
        ])
        .output();
    let Ok(o) = out else {
        return (String::new(), String::new());
    };
    if !o.status.success() {
        return (String::new(), String::new());
    }
    let text = String::from_utf8_lossy(&o.stdout);
    let mut parts = text.split('|');
    let brand = parts.next().unwrap_or("").trim().to_string();
    let market = parts.next().unwrap_or("").trim().to_string();
    (brand, market)
}

pub fn list_devices(adb: &Path) -> Vec<AndroidDevice> {
    let out = match silent_cmd(adb).args(["devices", "-l"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let text = String::from_utf8_lossy(&out.stdout);
    let mut devices = Vec::new();

    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('*') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        let Some(status) = parts.next() else { continue };
        if !matches!(
            status,
            "device" | "unauthorized" | "offline" | "recovery" | "sideload"
        ) {
            continue;
        }

        let mut model = String::new();
        for tok in parts {
            if let Some(m) = tok.strip_prefix("model:") {
                model = m.replace('_', " ");
            }
        }
        if model.is_empty() {
            model = if status == "unauthorized" {
                "Menunggu Izin di HP".to_string()
            } else {
                "Android Device".to_string()
            };
        }

        let (mut brand, market_name) = if status == "device" {
            fetch_brand_and_market(adb, serial)
        } else {
            (String::new(), String::new())
        };
        if brand.is_empty() {
            brand = guess_brand_from_model(&model);
        }

        let is_wifi = (serial.contains(':') || serial.contains("_adb-tls-connect"))
            && !serial.starts_with("emulator-");
        devices.push(AndroidDevice {
            serial: serial.to_string(),
            status: status.to_string(),
            model,
            brand,
            market_name,
            is_wifi,
        });
    }

    devices.sort_by_key(|d| if d.status == "device" { 0 } else { 1 });
    devices
}

pub fn install_scrcpy() -> Result<PathBuf, String> {
    let target_dir = local_bin_dir();
    fs::create_dir_all(&target_dir)
        .map_err(|e| format!("Gagal membuat folder {}: {e}", target_dir.display()))?;

    let target_str = target_dir.to_string_lossy().replace('\'', "''");
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$release = Invoke-RestMethod -Uri 'https://api.github.com/repos/Genymobile/scrcpy/releases/latest' -Headers @{{ 'User-Agent' = 'android-mirror-app' }}
$asset = $release.assets | Where-Object {{ $_.name -match '^scrcpy-win64-v.*\.zip$' }} | Select-Object -First 1
if (-not $asset) {{ throw 'Asset scrcpy-win64 tidak ditemukan di rilis terbaru.' }}
$zipPath = Join-Path $env:TEMP 'scrcpy-win64-latest.zip'
$extractTmp = Join-Path $env:TEMP 'scrcpy-extract-tmp'
if (Test-Path $extractTmp) {{ Remove-Item -Recurse -Force $extractTmp }}
Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zipPath -UseBasicParsing
Expand-Archive -Path $zipPath -DestinationPath $extractTmp -Force
$inner = Get-ChildItem -Path $extractTmp -Directory | Select-Object -First 1
$src = if ($inner) {{ $inner.FullName }} else {{ $extractTmp }}
Copy-Item -Path (Join-Path $src '*') -Destination '{target_str}' -Recurse -Force
Remove-Item -Force -ErrorAction SilentlyContinue $zipPath
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $extractTmp
"#
    );

    let mut cmd = Command::new("powershell");
    cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let out = cmd
        .output()
        .map_err(|e| format!("Gagal menjalankan PowerShell: {e}"))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if err.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            err
        };
        return Err(format!("Download/ekstrak scrcpy gagal: {msg}"));
    }

    let exe = target_dir.join("scrcpy.exe");
    if exe.is_file() {
        Ok(exe)
    } else {
        Err("scrcpy.exe tidak ditemukan setelah ekstrak.".to_string())
    }
}

fn get_wifi_ip(adb: &Path, serial: &str) -> Result<String, String> {
    let out = silent_cmd(adb)
        .args(["-s", serial, "shell", "ip", "route"])
        .output()
        .map_err(|e| format!("Gagal membaca IP HP: {e}"))?;

    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if let Some(pos) = tokens.iter().position(|&t| t == "src") {
            if let Some(ip) = tokens.get(pos + 1) {
                if !ip.starts_with("127.") {
                    return Ok((*ip).to_string());
                }
            }
        }
    }

    Err("IP Wi-Fi HP tidak ditemukan. Pastikan HP terhubung ke Wi-Fi yang sama dengan PC.".to_string())
}

pub fn switch_to_wifi(adb: &Path, serial: &str) -> Result<String, String> {
    let ip = get_wifi_ip(adb, serial)?;

    let tcpip = silent_cmd(adb)
        .args(["-s", serial, "tcpip", "5555"])
        .output()
        .map_err(|e| format!("Gagal mengaktifkan mode tcpip: {e}"))?;

    if !tcpip.status.success() {
        let err = String::from_utf8_lossy(&tcpip.stderr).trim().to_string();
        return Err(format!("adb tcpip gagal: {err}"));
    }

    thread::sleep(Duration::from_millis(1200));
    let addr = format!("{ip}:5555");
    connect_wifi(adb, &addr)?;
    Ok(addr)
}

pub fn connect_wifi(adb: &Path, addr: &str) -> Result<String, String> {
    let addr = addr.trim();
    if addr.is_empty() {
        return Err("Masukkan alamat IP:Port HP terlebih dahulu.".to_string());
    }
    let normalized = if addr.contains(':') {
        addr.to_string()
    } else {
        format!("{addr}:5555")
    };

    let out = silent_cmd(adb)
        .args(["connect", &normalized])
        .output()
        .map_err(|e| format!("Gagal menjalankan adb connect: {e}"))?;

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();

    if out.status.success() && (stdout.contains("connected") || stdout.contains("already connected")) {
        Ok(normalized)
    } else {
        let detail = if stdout.is_empty() { stderr } else { stdout };
        Err(format!("Gagal terhubung ke {normalized}: {detail}"))
    }
}

pub fn start_mirror(
    scrcpy: &Path,
    adb: Option<&Path>,
    opts: &MirrorOptions,
) -> Result<Child, String> {
    let mut cmd = Command::new(scrcpy);
    if let Some(adb_path) = adb {
        cmd.env("ADB", adb_path);
    }

    cmd.args(["-s", &opts.serial, "--window-title", &opts.window_title]);

    match opts.quality {
        0 => {
            cmd.args(["-m", "1280", "-b", "4M", "--max-fps", "30"]);
        }
        1 => {
            cmd.args(["-m", "1920", "-b", "8M", "--max-fps", "60"]);
        }
        _ => {
            cmd.args(["-b", "16M", "--max-fps", "60"]);
        }
    }

    if opts.turn_screen_off {
        cmd.arg("--turn-screen-off");
    }
    if opts.stay_awake {
        cmd.arg("--stay-awake");
    }
    if opts.always_on_top {
        cmd.arg("--always-on-top");
    }
    if !opts.forward_audio {
        cmd.arg("--no-audio");
    }
    if opts.rounded_window {
        cmd.arg("--window-borderless");
    }

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Gagal menjalankan scrcpy: {e}"))?;

    #[cfg(windows)]
    {
        let pid = child.id();
        let rounded = opts.rounded_window;
        let title = opts.window_title.clone();
        let topmost = opts.always_on_top;
        thread::spawn(move || crate::win::apply_window_style_loop(pid, rounded, title, topmost));
    }

    Ok(child)
}

pub fn wake_device(adb: &Path, serial: &str) -> Result<(), String> {
    let out = silent_cmd(adb)
        .args(["-s", serial, "shell", "input", "keyevent", "KEYCODE_WAKEUP"])
        .output()
        .map_err(|e| format!("Gagal membangunkan HP: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!("Gagal membangunkan HP: {err}"))
    }
}

pub fn fetch_device_metrics(adb: &Path, serial: &str) -> DeviceMetrics {
    let script = concat!(
        "dumpsys battery | grep -E 'level:|temperature:'; ",
        "echo '---CPU---'; ",
        "top -n 1 -d 0.2 -b 2>/dev/null | head -n 6; ",
        "echo '---CPUI---'; ",
        "dumpsys cpuinfo 2>/dev/null | grep -i 'TOTAL:' | tail -n 1; ",
        "echo '---GPU---'; ",
        "cat /sys/class/kgsl/kgsl-3d0/gpu_busy_percentage 2>/dev/null || ",
        "cat /sys/module/ged/parameters/gpu_utilization 2>/dev/null || ",
        "cat /sys/class/misc/mali0/device/utilization 2>/dev/null; true"
    );

    let Ok(out) = silent_cmd(adb)
        .args(["-s", serial, "shell", script])
        .output()
    else {
        return DeviceMetrics::default();
    };
    if out.stdout.is_empty() {
        return DeviceMetrics::default();
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let mut metrics = DeviceMetrics::default();
    let mut section = "BAT";

    for raw in text.lines() {
        let line = raw.trim();
        if line == "---CPU---" {
            section = "CPU";
            continue;
        } else if line == "---CPUI---" {
            section = "CPUI";
            continue;
        } else if line == "---GPU---" {
            section = "GPU";
            continue;
        }
        if line.is_empty() {
            continue;
        }

        match section {
            "BAT" => {
                if let Some(val) = line.strip_prefix("level:") {
                    if let Ok(pct) = val.trim().parse::<u8>() {
                        metrics.battery_pct = Some(pct.min(100));
                    }
                } else if let Some(val) = line.strip_prefix("temperature:") {
                    if let Ok(tenths) = val.trim().parse::<i32>() {
                        metrics.battery_temp_c = Some(tenths as f32 / 10.0);
                    }
                }
            }
            "CPU" => {
                if metrics.cpu_pct.is_none() && line.contains("%cpu") && line.contains("%idle") {
                    let mut total: Option<f32> = None;
                    let mut idle: Option<f32> = None;
                    for tok in line.split_whitespace() {
                        if let Some(num) = tok.strip_suffix("%cpu") {
                            total = num.parse::<f32>().ok();
                        } else if let Some(num) = tok.strip_suffix("%idle") {
                            idle = num.parse::<f32>().ok();
                        }
                    }
                    if let (Some(t), Some(i)) = (total, idle) {
                        if t > 0.0 {
                            let used = ((t - i).max(0.0) / t * 100.0).round() as u32;
                            metrics.cpu_pct = Some(used.min(100));
                        }
                    }
                }
            }
            "CPUI" => {
                if metrics.cpu_pct.is_none() && line.to_ascii_uppercase().contains("TOTAL:") {
                    if let Some(tok) = line.split('%').next() {
                        if let Ok(v) = tok.trim().parse::<f32>() {
                            metrics.cpu_pct = Some((v.round() as u32).min(100));
                        }
                    }
                }
            }
            "GPU" => {
                if metrics.gpu_pct.is_none() {
                    let digits: String = line
                        .chars()
                        .skip_while(|c| !c.is_ascii_digit())
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(v) = digits.parse::<u32>() {
                        metrics.gpu_pct = Some(v.min(100));
                    }
                }
            }
            _ => {}
        }
    }

    metrics
}
