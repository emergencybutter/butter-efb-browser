//! `--install` / `--uninstall`: deploy the EFB package to the Community folder and register the
//! companion in the sim's EXE.xml so that MSFS 2024 starts it (with `--exit-with-sim`).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use regex::Regex;

use crate::config::data_dir;
use crate::win;

include!(concat!(env!("OUT_DIR"), "/efb_package.rs"));

pub const ADDON_NAME: &str = "EFB Browser Companion";
pub const SIM_PROCESS: &str = "FlightSimulator2024.exe";
pub const EXIT_WITH_SIM: &str = "--exit-with-sim";
const PACKAGE_NAME: &str = "buttersim-efb-browser";
const EXE_NAME: &str = "efb-browser-companion.exe";

/// Collects the install report: printed as it goes, and saved to install.log.
struct Report(String);

impl Report {
    fn line(&mut self, s: impl AsRef<str>) {
        println!("{}", s.as_ref());
        let _ = writeln!(self.0, "{}", s.as_ref());
    }
    fn save(&self) {
        let dir = data_dir();
        let _ = fs::create_dir_all(&dir);
        self.save_to(&dir.join("install.log"));
    }
    fn save_to(&self, path: &Path) {
        let _ = fs::write(path, &self.0);
    }
}

struct Sim {
    label: &'static str,
    /// Folder holding UserCfg.opt and EXE.xml.
    config_dir: PathBuf,
}

/// The sim installs that have a UserCfg.opt for this user. One that cannot even be read still counts
/// (Store data moved to another drive goes through a junction into a protected `WpSystem` folder):
/// the install step then retries it with administrator rights, or reports why it failed.
fn find_sims() -> Vec<Sim> {
    let mut candidates = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push((
            "Microsoft Store",
            PathBuf::from(local).join(r"Packages\Microsoft.Limitless_8wekyb3d8bbwe\LocalCache"),
        ));
    }
    if let Some(roaming) = std::env::var_os("APPDATA") {
        candidates.push(("Steam", PathBuf::from(roaming).join("Microsoft Flight Simulator 2024")));
    }
    candidates
        .into_iter()
        .filter(|(_, dir)| !matches!(load_user_cfg(dir), Ok(None)))
        .map(|(label, config_dir)| Sim { label, config_dir })
        .collect()
}

/// The part of UserCfg.opt we use: the Community folder (`None` if InstalledPackagesPath is missing).
struct UserCfg {
    community: Option<PathBuf>,
}

/// `Ok(None)` when there is no UserCfg.opt (sim not installed, or never started).
fn load_user_cfg(config_dir: &Path) -> std::io::Result<Option<UserCfg>> {
    let cfg = match fs::read(config_dir.join("UserCfg.opt")) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let cfg = String::from_utf8_lossy(&cfg);
    let community = Regex::new(r#"(?m)^\s*InstalledPackagesPath\s+"([^"]+)""#)
        .unwrap()
        .captures(&cfg)
        .map(|c| PathBuf::from(&c[1]).join("Community"));
    Ok(Some(UserCfg { community }))
}

fn user_cfg_or_error(config_dir: &Path) -> Result<Option<UserCfg>> {
    load_user_cfg(config_dir).with_context(|| format!("cannot read {}", config_dir.join("UserCfg.opt").display()))
}

/// True when Windows refused access somewhere in the error chain (administrator rights may help).
fn access_denied(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

/// EXE.xml path, keeping the spelling of an existing file.
fn exe_xml_path(config_dir: &Path) -> PathBuf {
    fs::read_dir(config_dir)
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case("exe.xml"))
                .map(|e| e.path())
        })
        .unwrap_or_else(|| config_dir.join("EXE.xml"))
}

pub fn install_dir() -> PathBuf {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    local.join("Programs").join(ADDON_NAME)
}

fn timestamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ------------------------------------------------------------------ EXE.xml editing

/// Text of an EXE.xml file, with what is needed to write it back the same way.
struct XmlText {
    text: String,
    bom: bool,
    /// Not valid UTF-8: decoded byte per char (Windows-1252 files), re-encoded the same way.
    latin1: bool,
}

impl XmlText {
    fn decode(bytes: &[u8]) -> XmlText {
        let (bom, body) = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
            Some(rest) => (true, rest),
            None => (false, bytes),
        };
        match std::str::from_utf8(body) {
            Ok(s) => XmlText { text: s.to_string(), bom, latin1: false },
            Err(_) => XmlText { text: body.iter().map(|&b| b as char).collect(), bom, latin1: true },
        }
    }

    fn encode(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        if self.bom {
            out.extend_from_slice(b"\xEF\xBB\xBF");
        }
        if self.latin1 {
            for c in self.text.chars() {
                let b = u8::try_from(u32::from(c))
                    .map_err(|_| anyhow!("the install path contains characters EXE.xml's encoding cannot store"))?;
                out.push(b);
            }
        } else {
            out.extend_from_slice(self.text.as_bytes());
        }
        Ok(out)
    }
}

const NEW_EXE_XML: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n\
<SimBase.Document Type=\"Launch\" version=\"1,0\">\r\n\
    <Descr>Launch</Descr>\r\n\
    <Filename>EXE.xml</Filename>\r\n\
    <Disabled>False</Disabled>\r\n\
    <Launch.ManualLoad>False</Launch.ManualLoad>\r\n\
</SimBase.Document>\r\n";

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Remove our `<Launch.Addon>` blocks (with their line). Returns how many were removed.
fn remove_entries(text: &mut String) -> usize {
    let block = Regex::new(r"(?is)<Launch\.Addon>.*?</Launch\.Addon>").unwrap();
    let ours = Regex::new(&format!(r"(?is)<Name>\s*{}\s*</Name>", regex::escape(ADDON_NAME))).unwrap();
    let ranges: Vec<(usize, usize)> = block
        .find_iter(text)
        .filter(|m| ours.is_match(m.as_str()))
        .map(|m| (m.start(), m.end()))
        .collect();
    for &(start, end) in ranges.iter().rev() {
        let bytes = text.as_bytes();
        let mut s = start;
        while s > 0 && matches!(bytes[s - 1], b' ' | b'\t') {
            s -= 1;
        }
        let mut e = end;
        if bytes.get(e) == Some(&b'\r') {
            e += 1;
        }
        if bytes.get(e) == Some(&b'\n') {
            e += 1;
        }
        text.replace_range(s..e, "");
    }
    ranges.len()
}

/// Add (or replace) our entry. Fails without changing anything if the file is not an EXE.xml.
fn upsert_entry(text: &mut String, exe: &Path) -> Result<()> {
    let close = Regex::new(r"(?i)</SimBase\.Document>").unwrap();
    if close.find(text).is_none() {
        bail!("not a valid EXE.xml (no </SimBase.Document>)");
    }
    remove_entries(text);
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let indent = Regex::new(r"(?m)^([ \t]+)<")
        .unwrap()
        .captures(text)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| "    ".into());
    let block = format!(
        "{i}<Launch.Addon>{eol}\
         {i}{i}<Name>{name}</Name>{eol}\
         {i}{i}<Disabled>False</Disabled>{eol}\
         {i}{i}<Path>{path}</Path>{eol}\
         {i}{i}<CommandLine>{args}</CommandLine>{eol}\
         {i}</Launch.Addon>{eol}",
        i = indent,
        name = ADDON_NAME,
        path = xml_escape(&exe.to_string_lossy()),
        args = EXIT_WITH_SIM,
    );
    let pos = close.find_iter(text).last().unwrap().start();
    // Insert at the start of the closing tag's line when it only has indentation before it.
    let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    if text[line_start..pos].trim().is_empty() {
        text.insert_str(line_start, &block);
    } else {
        text.insert_str(pos, &format!("{eol}{block}"));
    }
    Ok(())
}

/// Apply `edit` to EXE.xml, writing a backup first. Returns false when nothing had to change.
fn edit_exe_xml(path: &Path, create: bool, edit: impl FnOnce(&mut String) -> Result<()>) -> Result<bool> {
    let original = match fs::read(path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let mut xml = match &original {
        Some(b) => XmlText::decode(b),
        None if create => XmlText { text: NEW_EXE_XML.into(), bom: false, latin1: false },
        None => return Ok(false),
    };
    let before = xml.text.clone();
    edit(&mut xml.text)?;
    if xml.text == before && original.is_some() {
        return Ok(false);
    }
    let bytes = xml.encode()?;
    if let Some(orig) = &original {
        let name = path.file_name().unwrap().to_string_lossy();
        let backup = path.with_file_name(format!("{name}.efb-browser.{}.bak", timestamp()));
        fs::write(&backup, orig).with_context(|| format!("cannot write backup {}", backup.display()))?;
    }
    let tmp = path.with_extension("xml.efb-browser.tmp");
    fs::write(&tmp, &bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(true)
}

// ------------------------------------------------------------------ steps

fn stop_running(exe: &Path, report: &mut Report) {
    let pids = win::find_processes_by_path(exe);
    for pid in &pids {
        win::terminate(*pid, 5000);
    }
    if !pids.is_empty() {
        report.line(format!("Stopped the running companion ({} process(es))", pids.len()));
        // Let the browser release the profile.
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn install_package(community: &Path) -> Result<PathBuf> {
    let target = community.join(PACKAGE_NAME);
    if target.exists() {
        fs::remove_dir_all(&target).with_context(|| format!("cannot remove the old {}", target.display()))?;
    }
    for (rel, data) in EFB_PACKAGE {
        let path = target.join(rel.replace('/', "\\"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        fs::write(&path, data).with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(target)
}

fn copy_self(target: &Path) -> Result<()> {
    let me = std::env::current_exe()?;
    let same = fs::canonicalize(&me).ok() == fs::canonicalize(target).ok();
    if same {
        return Ok(());
    }
    fs::create_dir_all(target.parent().unwrap())?;
    fs::copy(&me, target).with_context(|| format!("cannot copy the companion to {}", target.display()))?;
    Ok(())
}

/// Options of `--install`, set by the MSI installer.
#[derive(Default)]
pub struct InstallOptions {
    /// `--replace-inno`: forget an install made by the earlier Inno Setup installer (same folder),
    /// so that only the MSI is listed in Settings > Apps.
    pub replace_inno: bool,
    /// `--error-dialog`: report a failure in a message box (the MSI only shows a generic error).
    pub error_dialog: bool,
    /// `--no-elevate`: never ask for administrator rights (silent installs).
    pub no_elevate: bool,
}

/// Uninstall key written by the Inno Setup installer (installer/EFBBrowser.iss: AppId + "_is1").
const INNO_UNINSTALL_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{6A08559A-D3A6-46AB-9994-0477F400A5BE}_is1";

pub fn install(opts: &InstallOptions) -> Result<()> {
    let mut r = Report(String::new());
    let result = install_steps(&mut r, opts);
    if let Err(e) = &result {
        r.line(format!("Error: {e:#}"));
    }
    r.save();
    if let (Err(e), true) = (&result, opts.error_dialog) {
        win::error_dialog(
            "EFB Web Browser setup",
            &format!(
                "The EFB Browser could not be set up for Microsoft Flight Simulator 2024:\n\n{e:#}\n\nDetails: {}",
                data_dir().join("install.log").display()
            ),
        );
    }
    result
}

/// The Inno uninstaller would run `--uninstall` and delete our exe: drop it and its Apps entry
/// instead (the MSI now owns the same files).
fn replace_inno(r: &mut Report) {
    let mut removed = win::delete_user_registry_key(INNO_UNINSTALL_KEY);
    let dir = install_dir();
    for name in ["unins000.exe", "unins000.dat"] {
        removed |= fs::remove_file(dir.join(name)).is_ok();
    }
    if removed {
        r.line("Replaced the previous (Inno Setup) installation");
    }
}

fn install_steps(r: &mut Report, opts: &InstallOptions) -> Result<()> {
    r.line(format!("Installing {ADDON_NAME}"));
    // The sim reads the Community folder only when it starts: replacing the package under a running
    // sim makes the Browser app vanish from the EFB until the next start.
    if !win::find_processes(SIM_PROCESS).is_empty() {
        bail!("Microsoft Flight Simulator 2024 is running. Quit the sim, then run the installer again.");
    }
    let sims = find_sims();
    if sims.is_empty() {
        bail!("Microsoft Flight Simulator 2024 was not found. Start the sim once, then run the installer again.");
    }
    let exe = install_dir().join(EXE_NAME);
    stop_running(&exe, r);
    copy_self(&exe)?;
    if opts.replace_inno {
        replace_inno(r);
    }
    r.line(format!("Companion: {}", exe.display()));

    let mut errors = Vec::new();
    for sim in &sims {
        r.line(format!("MSFS 2024 ({}):", sim.label));
        let mut result = install_sim(&sim.config_dir, &exe, r);
        if let Err(e) = &result {
            if access_denied(e) && !opts.no_elevate && !win::is_elevated() {
                r.line(format!("  {e:#}"));
                r.line("  Access denied: retrying with administrator rights");
                let (dir, exe) = (sim.config_dir.to_string_lossy(), exe.to_string_lossy());
                result = run_elevated(&["--install-sim", &dir, "--companion", &exe], r);
            }
        }
        if let Err(e) = result {
            r.line(format!("  Failed: {e:#}"));
            errors.push(format!("MSFS 2024 ({}): {e:#}", sim.label));
        }
    }
    if !errors.is_empty() {
        bail!(errors.join("\n"));
    }

    r.line("The companion will start with the sim.");
    r.line("Done.");
    Ok(())
}

/// Deploy the EFB package to one sim's Community folder and register the companion in its EXE.xml.
fn install_sim(config_dir: &Path, exe: &Path, r: &mut Report) -> Result<()> {
    let cfg = user_cfg_or_error(config_dir)?
        .ok_or_else(|| anyhow!("{} not found", config_dir.join("UserCfg.opt").display()))?;
    match (&cfg.community, EFB_PACKAGE.is_empty()) {
        (_, true) => r.line("  EFB app: not included in this build, skipped"),
        (Some(community), false) => {
            let target = install_package(community)?;
            r.line(format!("  EFB app: {}", target.display()));
        }
        (None, false) => r.line("  EFB app: Community folder not found in UserCfg.opt, skipped"),
    }
    let xml = exe_xml_path(config_dir);
    edit_exe_xml(&xml, true, |t| upsert_entry(t, exe))?;
    r.line(format!("  Auto-start: {}", xml.display()));
    Ok(())
}

/// Remove one sim's EXE.xml entry and EFB package. Tries both; returns the first error.
fn uninstall_sim(config_dir: &Path, r: &mut Report) -> Result<()> {
    let Some(cfg) = user_cfg_or_error(config_dir)? else { return Ok(()) };
    let mut errors: Vec<anyhow::Error> = Vec::new();
    let xml = exe_xml_path(config_dir);
    match edit_exe_xml(&xml, false, |t| {
        remove_entries(t);
        Ok(())
    }) {
        Ok(true) => r.line(format!("  Removed auto-start from {}", xml.display())),
        Ok(false) => {}
        Err(e) => errors.push(e),
    }
    if let Some(target) = cfg.community.map(|c| c.join(PACKAGE_NAME)) {
        if target.exists() {
            match fs::remove_dir_all(&target) {
                Ok(()) => r.line(format!("  Removed {}", target.display())),
                Err(e) => errors.push(anyhow::Error::new(e).context(format!("cannot remove {}", target.display()))),
            }
        }
    }
    for e in errors.iter().skip(1) {
        r.line(format!("  {e:#}"));
    }
    errors.into_iter().next().map_or(Ok(()), Err)
}

// ------------------------------------------------------------------ administrator retry

/// Run this exe again with administrator rights (UAC prompt) and `args`, wait for it, and copy its
/// report into ours.
fn run_elevated(args: &[&str], r: &mut Report) -> Result<()> {
    let log = std::env::temp_dir().join(format!("efb-browser-elevated-{}.log", std::process::id()));
    let _ = fs::remove_file(&log);
    let log_arg = log.to_string_lossy();
    let params = args
        .iter()
        .copied()
        .chain(["--log", &log_arg])
        // Paths cannot contain quotes; a trailing backslash would escape the closing one.
        .map(|a| format!("\"{}\"", a.trim_end_matches('\\')))
        .collect::<Vec<_>>()
        .join(" ");
    let me = std::env::current_exe()?;
    let code = win::run_elevated(&me, &params).map_err(|e| match e.raw_os_error() {
        Some(1223) => anyhow!("administrator permission was not given"), // ERROR_CANCELLED
        _ => anyhow!("cannot run with administrator rights: {e}"),
    })?;
    let output = fs::read_to_string(&log).unwrap_or_default();
    let _ = fs::remove_file(&log);
    let mut error = None;
    for line in output.lines() {
        r.line(line);
        if let Some(e) = line.trim().strip_prefix("Error: ") {
            error = Some(e.to_string());
        }
    }
    if code != 0 {
        bail!(error.unwrap_or_else(|| format!("the administrator step failed (exit code {code})")));
    }
    Ok(())
}

/// The elevated process started by `run_elevated`: `--install-sim <config dir> --companion <exe>` or
/// `--uninstall-sim <config dir>`, with `--log <file>`. Returns the exit code.
pub fn elevated_helper(args: &[String]) -> i32 {
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let mut r = Report(String::new());
    let result = match (value("--install-sim"), value("--uninstall-sim"), value("--companion")) {
        (Some(dir), _, Some(exe)) => install_sim(&dir, &exe, &mut r),
        (None, Some(dir), _) => uninstall_sim(&dir, &mut r),
        _ => Err(anyhow!("bad arguments")),
    };
    if let Err(e) = &result {
        r.line(format!("  Error: {e:#}"));
    }
    if let Some(log) = value("--log") {
        r.save_to(&log);
    }
    i32::from(result.is_err())
}

pub fn uninstall() -> Result<()> {
    let mut r = Report(String::new());
    let result = uninstall_steps(&mut r);
    if let Err(e) = &result {
        r.line(format!("Error: {e:#}"));
    }
    r.save();
    result
}

fn uninstall_steps(r: &mut Report) -> Result<()> {
    r.line(format!("Uninstalling {ADDON_NAME}"));
    stop_running(&install_dir().join(EXE_NAME), r);
    let mut errors = Vec::new();
    for sim in find_sims() {
        r.line(format!("MSFS 2024 ({}):", sim.label));
        let mut result = uninstall_sim(&sim.config_dir, r);
        if let Err(e) = &result {
            if access_denied(e) && !win::is_elevated() {
                r.line(format!("  {e:#}"));
                r.line("  Access denied: retrying with administrator rights");
                result = run_elevated(&["--uninstall-sim", &sim.config_dir.to_string_lossy()], r);
            }
        }
        if let Err(e) = result {
            errors.push(format!("MSFS 2024 ({}): {e:#}", sim.label));
        }
    }
    if !errors.is_empty() {
        bail!(errors.join("; "));
    }
    r.line("Done. Browser data (cookies, logins) is kept in ".to_string() + &data_dir().display().to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<SimBase.Document Type=\"Launch\" version=\"1,0\">\r\n    <Descr>Launch</Descr>\r\n    <Filename>EXE.xml</Filename>\r\n    <Disabled>False</Disabled>\r\n    <Launch.ManualLoad>False</Launch.ManualLoad>\r\n    <Launch.Addon>\r\n        <Name>FenixA320</Name>\r\n        <Disabled>false</Disabled>\r\n        <Path>C:\\Program Files\\FenixSim A320\\deps\\FenixBootstrapper.exe</Path>\r\n    </Launch.Addon>\r\n    <Launch.Addon>\r\n        <Name>IVAO Pilot Client</Name>\r\n        <Disabled>False</Disabled>\r\n        <Path>C:\\IVAO\\Pilot\\PilotCore\\64\\pilot_core_msfs.exe</Path>\r\n        <Commandline />\r\n    </Launch.Addon>\r\n</SimBase.Document>\r\n";

    #[test]
    fn install_then_uninstall_restores_file() {
        let mut t = REAL.to_string();
        upsert_entry(&mut t, Path::new(r"C:\Users\a & b\Programs\EFB Browser Companion\efb-browser-companion.exe")).unwrap();
        assert!(t.contains("    <Launch.Addon>\r\n        <Name>EFB Browser Companion</Name>"));
        assert!(t.contains(r"<Path>C:\Users\a &amp; b\Programs"));
        assert!(t.contains("<CommandLine>--exit-with-sim</CommandLine>\r\n    </Launch.Addon>\r\n</SimBase.Document>"));
        assert!(t.contains("FenixA320") && t.contains("IVAO Pilot Client"));
        // Installing twice keeps a single entry.
        upsert_entry(&mut t, Path::new(r"C:\x.exe")).unwrap();
        assert_eq!(t.matches("EFB Browser Companion").count(), 1);
        assert_eq!(remove_entries(&mut t), 1);
        assert_eq!(t, REAL);
    }

    #[test]
    fn new_file_and_errors() {
        let mut t = NEW_EXE_XML.to_string();
        upsert_entry(&mut t, Path::new(r"C:\x.exe")).unwrap();
        assert!(t.ends_with("    </Launch.Addon>\r\n</SimBase.Document>\r\n"));
        let mut bad = "<foo/>".to_string();
        assert!(upsert_entry(&mut bad, Path::new(r"C:\x.exe")).is_err());
        assert_eq!(bad, "<foo/>");
    }

    #[test]
    fn latin1_round_trip() {
        let bytes = b"<?xml version=\"1.0\" encoding=\"Windows-1252\"?>\n<SimBase.Document><Descr>caf\xE9</Descr>\n</SimBase.Document>\n";
        let mut x = XmlText::decode(bytes);
        assert!(x.latin1);
        upsert_entry(&mut x.text, Path::new("C:\\Users\\Andr\u{e9}\\c.exe")).unwrap();
        let out = x.encode().unwrap();
        assert!(out.windows(4).any(|w| w == b"caf\xE9"));
        assert!(out.windows(6).any(|w| w == b"Andr\xE9\\"));
    }
}
