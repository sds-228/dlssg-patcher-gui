use std::os::windows::process::CommandExt;

use serde::{Deserialize, Serialize};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// GPU architecture detected for the supported-family gate and patch display.
/// (The mod's own `Router=Auto` resolves SM75/SM86 from the physical GPU, so no
/// INI rewrite happens; see patcher.rs.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Arch {
    /// RTX 20-series / Turing (SM7.5) - supported by dlssg_for_sm86.
    Sm75,
    /// RTX 30-series / Ampere (SM8.6).
    Sm86,
    /// Not supported by dlssg_for_sm86.
    Unsupported,
}

impl Arch {
    pub fn router(&self) -> &'static str {
        match self {
            Arch::Sm75 => "SM75",
            Arch::Sm86 => "SM86",
            Arch::Unsupported => "",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub name: String,
    pub driver: String,
    pub vram_mib: Option<u64>,
    pub arch: Arch,
    /// True when the card was found through the WMI fallback because
    /// `nvidia-smi` could not query it - e.g. a hybrid laptop whose display is
    /// currently on the integrated GPU.
    pub from_wmi: bool,
    /// When `from_wmi`, the integrated adapter's name if one was present.
    pub igpu: Option<String>,
}

/// Model-name markers per architecture (substring match, case-insensitive).
/// Consumer and prosumer cards only; datacenter/GeForce GTX are left unsupported.
const SM75_MARKERS: &[&str] = &[
    "2080 TI",
    "2080 SUPER",
    "2080",
    "2070 SUPER",
    "2070",
    "2060 SUPER",
    "2060",
    "TITAN RTX",
    "QUADRO RTX",
];

const SM86_MARKERS: &[&str] = &[
    "3090 TI",
    "3090",
    "3080 TI",
    "3080",
    "3070 TI",
    "3070",
    "3060 TI",
    "3060",
    "3050",
    "2050",
    "RTX A500",
    "RTX A1000",
    "RTX A2000",
    "RTX A3000",
    "RTX A4000",
    "RTX A4500",
    "RTX A5000",
    "RTX A5500",
    "RTX A6000",
];

pub fn infer_arch(name: &str) -> Arch {
    let up = name.to_uppercase();
    if SM75_MARKERS.iter().any(|m| up.contains(m)) {
        Arch::Sm75
    } else if SM86_MARKERS.iter().any(|m| up.contains(m)) {
        Arch::Sm86
    } else {
        Arch::Unsupported
    }
}

/// Query the primary GPU via `nvidia-smi`, falling back to a Windows adapter
/// inventory (`Win32_VideoController` plus PnP display devices) on hybrid laptops
/// where the dGPU is powered down and only the integrated GPU drives the display.
pub fn query_gpu() -> Result<GpuInfo, String> {
    match query_nvidia_smi() {
        Ok(info) => Ok(info),
        Err(smi_err) => {
            // On a hybrid laptop the NVIDIA card can be powered down (the display
            // is on the iGPU, or G-Helper/Optimus has put the dGPU to sleep), so
            // nvidia-smi fails even though an RTX card is physically present.
            // Consult the Windows adapter inventory so the user is guided instead
            // of locked out of patching.
            if let Some((name, arch, igpu)) = pick_hybrid_gpu(&query_adapters()) {
                return Ok(GpuInfo {
                    name,
                    driver: String::new(),
                    vram_mib: None,
                    arch,
                    from_wmi: true,
                    igpu,
                });
            }
            Err(smi_err)
        }
    }
}

fn query_nvidia_smi() -> Result<GpuInfo, String> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name,driver_version,memory.total", "--format=csv,noheader,nounits"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("could not run nvidia-smi ({e}); is an NVIDIA driver installed?"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "nvidia-smi failed: {}",
            stderr.trim().lines().next().unwrap_or("no nvidia GPU found")
        ));
    }

    let text = String::from_utf8_lossy(&out.stdout);
    parse_nvidia_smi(&text)
}

/// Parse `nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv,noheader,nounits`
/// output. On multi-GPU desktops, prefer the first supported RTX 20/30 card over an
/// unsupported display adapter in slot 0; fall back to the first line if none is supported.
pub(crate) fn parse_nvidia_smi(stdout: &str) -> Result<GpuInfo, String> {
    let mut first_gpu: Option<GpuInfo> = None;
    for line in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut parts = line.split(',');
        let name = parts.next().unwrap_or("Unknown").trim().to_string();
        let driver = parts.next().unwrap_or("?").trim().to_string();
        let vram = parts.next().and_then(|v| v.trim().parse::<u64>().ok());
        let arch = infer_arch(&name);
        let info = GpuInfo {
            name,
            driver,
            vram_mib: vram,
            arch,
            from_wmi: false,
            igpu: None,
        };
        if arch != Arch::Unsupported {
            return Ok(info);
        }
        if first_gpu.is_none() {
            first_gpu = Some(info);
        }
    }
    first_gpu.ok_or_else(|| "nvidia-smi returned no output".to_string())
}

/// Inventory the graphics adapters for the `nvidia-smi` fallback.
///
/// Two Windows sources are merged because they disagree about a dGPU that has
/// been powered down (G-Helper "Eco" mode, or Optimus runtime D3): the live
/// `Win32_VideoController` list drops it entirely, while the PnP display-device
/// list still reports it (as `Unknown`/phantom). Either source failing is
/// non-fatal - the other, and ultimately nvidia-smi's own error, still decide.
fn query_adapters() -> Vec<Adapter> {
    let mut adapters = Vec::new();
    if let Ok(mut a) = query_video_controllers() {
        adapters.append(&mut a);
    }
    if let Ok(mut a) = query_pnp_display_adapters() {
        adapters.append(&mut a);
    }
    adapters
}

/// The live display controllers, via `Win32_VideoController`. Unlike `nvidia-smi`
/// this lists the integrated GPU too, so it works on a laptop whose NVIDIA card
/// is merely asleep.
fn query_video_controllers() -> Result<Vec<Adapter>, String> {
    let stdout = run_powershell(
        "Get-CimInstance Win32_VideoController | ForEach-Object { \"$($_.Name)|$($_.DriverVersion)|$($_.Status)|$($_.PNPDeviceID)\" }",
    )?;
    Ok(parse_adapters(&stdout))
}

/// Every display-class PnP device, including powered-down ones. `Get-PnpDevice`
/// (unlike `Win32_VideoController`) still lists a dGPU that Windows has put into
/// a not-present state, which is exactly the hybrid case we are guiding for.
fn query_pnp_display_adapters() -> Result<Vec<Adapter>, String> {
    let stdout = run_powershell(
        "Get-PnpDevice -Class Display -ErrorAction SilentlyContinue | ForEach-Object { \"$($_.FriendlyName)||$($_.Status)|$($_.InstanceId)\" }",
    )?;
    Ok(parse_adapters(&stdout))
}

/// Run a short PowerShell script and return its stdout. Failures become `Err` so
/// each inventory source can be treated as best-effort.
fn run_powershell(script: &str) -> Result<String, String> {
    // Force UTF-8 stdout: Windows PowerShell 5.1 otherwise writes the OEM codepage,
    // which mangles non-ASCII adapter names.
    let script = format!("try {{ [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 }} catch {{}}; {script}");
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("could not run PowerShell ({e})"))?;

    if !out.status.success() {
        return Err("PowerShell GPU query failed".to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One graphics adapter from the merged Windows inventory
/// (`Win32_VideoController` plus `Get-PnpDevice -Class Display`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    pub name: String,
    /// Driver version; empty for adapters that only the PnP source reported.
    pub driver: String,
    /// `OK` (or empty when the column is absent) means the adapter is enabled
    /// and working. The PnP source can also report `Unknown` (a powered-down
    /// dGPU, e.g. G-Helper "Eco"/Optimus runtime D3) or `Error` (disabled).
    pub status: String,
    /// The device instance path (e.g. `PCI\VEN_10DE&DEV_249C&...`), used to
    /// identify the vendor by PCI ID. Empty when the source did not report one.
    pub instance_id: String,
}

/// Parse adapter-inventory output emitted as one `Name|Driver|Status|InstanceId`
/// line per adapter (the PnP source has an empty driver). Blank lines, lines
/// without a `|`, and lines with an empty name are skipped; a trailing column may
/// be absent.
pub fn parse_adapters(stdout: &str) -> Vec<Adapter> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let (name, rest) = line.split_once('|')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            let mut fields = rest.split('|');
            let driver = fields.next().unwrap_or("").trim();
            let status = fields.next().unwrap_or("").trim();
            let instance_id = fields.next().unwrap_or("").trim();
            Some(Adapter {
                name: name.to_string(),
                driver: driver.to_string(),
                status: status.to_string(),
                instance_id: instance_id.to_string(),
            })
        })
        .collect()
}

/// Is this adapter an NVIDIA GPU? Prefer the PCI vendor ID from the instance
/// path (`VEN_10DE`), which survives a powered-down or disabled device and does
/// not depend on the display name; fall back to the name for a source that
/// reports no instance path.
fn is_nvidia(adapter: &Adapter) -> bool {
    adapter.instance_id.to_uppercase().contains("VEN_10DE")
        || adapter.name.to_uppercase().contains("NVIDIA")
}

/// Pick a supported NVIDIA card (RTX 20/30 series) from an adapter inventory,
/// along with the name of an integrated adapter if present.
///
/// A working adapter is preferred. When every NVIDIA card reported is currently
/// not `OK` - a dGPU powered down by G-Helper "Eco" mode or Optimus runtime D3,
/// which Windows reports as `Unknown` (phantom), or `Error` if it is disabled -
/// the card is still selected: patching is a file operation, so the user is
/// guided by the amber card rather than locked out.
pub fn pick_hybrid_gpu(adapters: &[Adapter]) -> Option<(String, Arch, Option<String>)> {
    let igpu = adapters
        .iter()
        .find(|a| {
            let up = a.name.to_uppercase();
            ["INTEL", "AMD", "RADEON", "VEGA", "UHD GRAPHICS"]
                .iter()
                .any(|marker| up.contains(marker))
        })
        .map(|a| a.name.clone());

    let nvidia = |working_only: bool| {
        adapters.iter().find_map(|a| {
            if !is_nvidia(a) {
                return None;
            }
            let working = a.status.is_empty() || a.status.eq_ignore_ascii_case("OK");
            if working_only && !working {
                return None;
            }
            let arch = infer_arch(&a.name);
            (arch != Arch::Unsupported).then(|| (a.name.clone(), arch))
        })
    };

    nvidia(true)
        .or_else(|| nvidia(false))
        .map(|(name, arch)| (name, arch, igpu.clone()))
}

/// Soft guidance on driver version. dlssg_for_sm86 declares a tested driver, not a minimum.
pub fn driver_note(driver: &str) -> Option<&'static str> {
    let major = driver.split('.').next()?.parse::<i64>().ok()?;
    if major < 500 {
        Some("Very old driver. Frame generation needs a recent NVIDIA driver. Update via GeForce Experience / NVIDIA App.")
    } else if major < 550 {
        Some("Older driver. dlssg_for_sm86 was validated on 591.86; update if you hit issues.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ad(name: &str) -> Adapter {
        Adapter { name: name.into(), driver: String::new(), status: "OK".into(), instance_id: String::new() }
    }

    #[test]
    fn parse_adapters_splits_name_driver_and_status() {
        let stdout = "Intel(R) UHD Graphics|31.0.101.2111|OK\r\nNVIDIA GeForce RTX 3080 Laptop GPU|31.0.15.3699|OK\r\n";
        let adapters = parse_adapters(stdout);
        assert_eq!(adapters.len(), 2);
        assert_eq!(adapters[0].name, "Intel(R) UHD Graphics");
        assert_eq!(adapters[0].driver, "31.0.101.2111");
        assert_eq!(adapters[0].status, "OK");
        assert_eq!(adapters[1].name, "NVIDIA GeForce RTX 3080 Laptop GPU");
        assert_eq!(adapters[1].driver, "31.0.15.3699");
        assert_eq!(adapters[0].instance_id, "", "a 3-column line has no instance path");
    }

    #[test]
    fn parse_adapters_tolerates_a_missing_status_column() {
        let adapters = parse_adapters("NVIDIA GeForce RTX 2060|552.22");
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].driver, "552.22");
        assert_eq!(adapters[0].status, "");
    }

    #[test]
    fn parse_adapters_skips_blank_and_unnamed_lines() {
        assert!(parse_adapters("\r\n   \r\nno-separator\r\n|driver\r\n").is_empty());
    }

    #[test]
    fn pick_hybrid_gpu_finds_dgpu_and_names_the_igpu() {
        let adapters = vec![ad("Intel(R) UHD Graphics"), ad("NVIDIA GeForce RTX 3080 Laptop GPU")];
        let (name, arch, igpu) = pick_hybrid_gpu(&adapters).expect("dGPU should be found");
        assert_eq!(name, "NVIDIA GeForce RTX 3080 Laptop GPU");
        assert_eq!(arch, Arch::Sm86);
        assert_eq!(igpu.as_deref(), Some("Intel(R) UHD Graphics"));
    }

    #[test]
    fn pick_hybrid_gpu_maps_20_series_to_sm75() {
        let (_, arch, igpu) = pick_hybrid_gpu(&[ad("NVIDIA GeForce RTX 2060")]).expect("dGPU");
        assert_eq!(arch, Arch::Sm75);
        assert_eq!(igpu, None);
    }

    #[test]
    fn pick_hybrid_gpu_is_none_when_only_an_igpu_is_present() {
        assert!(pick_hybrid_gpu(&[ad("AMD Radeon(TM) Graphics")]).is_none());
    }

    #[test]
    fn pick_hybrid_gpu_ignores_nvidia_outside_the_20_30_range() {
        assert!(pick_hybrid_gpu(&[ad("NVIDIA GeForce RTX 4090")]).is_none());
    }

    #[test]
    fn pick_hybrid_gpu_accepts_a_disabled_dgpu() {
        // A card disabled by G-Helper, or one Windows reports with an error, must
        // not lock the user out of patching.
        let adapters = vec![Adapter {
            name: "NVIDIA GeForce RTX 3060 Laptop GPU".into(),
            driver: "31.0.15.3699".into(),
            status: "Error".into(),
            instance_id: r"PCI\VEN_10DE&DEV_2519&SUBSYS_16221043".into(),
        }];
        assert!(pick_hybrid_gpu(&adapters).is_some());
    }

    #[test]
    fn pick_hybrid_gpu_accepts_a_powered_down_dgpu() {
        // G-Helper "Eco" mode (or Optimus runtime D3) powers the dGPU down, so
        // Windows only reports it through the PnP display inventory, as `Unknown`
        // (phantom). It must still be selectable so the user is guided, not locked
        // out of patching.
        let adapters = vec![
            ad("AMD Radeon(TM) Graphics"),
            Adapter {
                name: "NVIDIA GeForce RTX 3080 Laptop GPU".into(),
                driver: String::new(),
                status: "Unknown".into(),
                instance_id: r"PCI\VEN_10DE&DEV_249C&SUBSYS_16221043&REV_A1\4&36097CE1&0&0009".into(),
            },
        ];
        let (name, arch, igpu) = pick_hybrid_gpu(&adapters).expect("powered-down dGPU should be found");
        assert_eq!(name, "NVIDIA GeForce RTX 3080 Laptop GPU");
        assert_eq!(arch, Arch::Sm86);
        assert_eq!(igpu.as_deref(), Some("AMD Radeon(TM) Graphics"));
    }

    #[test]
    fn pick_hybrid_gpu_prefers_a_working_adapter_over_a_powered_down_one() {
        let adapters = vec![
            Adapter {
                name: "NVIDIA GeForce RTX 3060 Laptop GPU".into(),
                driver: String::new(),
                status: "Unknown".into(),
                instance_id: r"PCI\VEN_10DE&DEV_2519&SUBSYS_16221043".into(),
            },
            Adapter {
                name: "NVIDIA GeForce RTX 3080 Laptop GPU".into(),
                driver: "31.0.15.3699".into(),
                status: "OK".into(),
                instance_id: r"PCI\VEN_10DE&DEV_249C&SUBSYS_16221043&REV_A1\4&36097CE1&0&0009".into(),
            },
        ];
        let (name, arch, _) = pick_hybrid_gpu(&adapters).expect("a working dGPU should win");
        assert_eq!(name, "NVIDIA GeForce RTX 3080 Laptop GPU");
        assert_eq!(arch, Arch::Sm86);
    }

    #[test]
    fn parse_adapters_reads_a_pnp_line_with_an_empty_driver() {
        // The PnP source emits `Name||Status|InstanceId` (no driver column).
        let adapters =
            parse_adapters("NVIDIA GeForce RTX 3080 Laptop GPU||Unknown|PCI\\VEN_10DE&DEV_249C");
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].name, "NVIDIA GeForce RTX 3080 Laptop GPU");
        assert_eq!(adapters[0].driver, "");
        assert_eq!(adapters[0].status, "Unknown");
        assert_eq!(adapters[0].instance_id, r"PCI\VEN_10DE&DEV_249C");
    }

    #[test]
    fn parse_adapters_reads_the_pnp_instance_id() {
        // The powered-down dGPU keeps its real instance path even while reported
        // as `Unknown`, so the PCI vendor ID survives the power state.
        let adapters = parse_adapters(
            "NVIDIA GeForce RTX 3080 Laptop GPU||Unknown|PCI\\VEN_10DE&DEV_249C&SUBSYS_16221043&REV_A1\\4&36097CE1&0&0009",
        );
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].status, "Unknown");
        assert_eq!(
            adapters[0].instance_id,
            r"PCI\VEN_10DE&DEV_249C&SUBSYS_16221043&REV_A1\4&36097CE1&0&0009"
        );
    }

    #[test]
    fn pick_hybrid_gpu_identifies_nvidia_by_pci_vendor_id() {
        // A card whose display name omits "NVIDIA" (odd OEM or localised naming)
        // must still be recognised from the vendor ID in its instance path.
        let adapters = vec![
            ad("AMD Radeon(TM) Graphics"),
            Adapter {
                name: "GeForce RTX 3080 Laptop GPU".into(),
                driver: String::new(),
                status: "Unknown".into(),
                instance_id: r"PCI\VEN_10DE&DEV_249C&SUBSYS_16221043&REV_A1\4&36097CE1&0&0009".into(),
            },
        ];
        let (name, arch, igpu) =
            pick_hybrid_gpu(&adapters).expect("a VEN_10DE device should be picked");
        assert_eq!(name, "GeForce RTX 3080 Laptop GPU");
        assert_eq!(arch, Arch::Sm86);
        assert_eq!(igpu.as_deref(), Some("AMD Radeon(TM) Graphics"));
    }

    #[test]
    fn rtx_2050_laptop_maps_to_sm86_and_multi_gpu_smi_picks_supported_card() {
        assert_eq!(infer_arch("NVIDIA GeForce RTX 2050 Laptop GPU"), Arch::Sm86);
        let smi = "NVIDIA GeForce GT 1030, 560.94, 2048\nNVIDIA GeForce RTX 3070, 560.94, 8192\n";
        let info = parse_nvidia_smi(smi).expect("should parse multi-GPU output");
        assert_eq!(info.name, "NVIDIA GeForce RTX 3070");
        assert_eq!(info.arch, Arch::Sm86);
    }

    #[test]
    fn pick_hybrid_gpu_does_not_read_a_non_nvidia_vendor_id_as_nvidia() {
        let adapters = vec![Adapter {
            name: "AMD Radeon(TM) Graphics".into(),
            driver: String::new(),
            status: "OK".into(),
            instance_id: r"PCI\VEN_1002&DEV_1638&SUBSYS_16221043&REV_C4\4&12C9051D&0&0041".into(),
        }];
        assert!(pick_hybrid_gpu(&adapters).is_none());
    }
}