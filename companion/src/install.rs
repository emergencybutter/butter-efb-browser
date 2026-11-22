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
        let _ = fs::write(dir.join("install.log"), &self.0);
    }
}

struct Sim {
    label: &'static str,
    /// Folder holding UserCfg.opt and EXE.xml.
    config_dir: PathBuf,
    community: Option<PathBuf>,
}

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
        .filter_map(|(label, dir)| {
            let cfg = fs::read(dir.join("UserCfg.opt")).ok()?;
            let cfg = String::from_utf8_lossy(&cfg);
            let community = Regex::new(r#"(?m)^\s*InstalledPackagesPath\s+"([^"]+)""#)
                .unwrap()
                .captures(&cfg)
                .map(|c| PathBuf::from(&c[1]).join("Community"));
            Some(Sim { label, config_dir: dir, community })
        })
        .collect()
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
            fs::create_dir_all(parent)?;
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

fn start_detached(exe: &Path) -> Result<()> {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(EXIT_WITH_SIM);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_ok() {
            return Ok(());
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().map(|_| ()).context("cannot start the companion")
}

pub fn install() -> Result<()> {
    let mut r = Report(String::new());
    let result = install_steps(&mut r);
    if let Err(e) = &result {
        r.line(format!("Error: {e:#}"));
    }
    r.save();
    result
}

fn install_steps(r: &mut Report) -> Result<()> {
    r.line(format!("Installing {ADDON_NAME}"));
    let sims = find_sims();
    if sims.is_empty() {
        bail!("Microsoft Flight Simulator 2024 was not found. Start the sim once, then run the installer again.");
    }
    let exe = install_dir().join(EXE_NAME);
    stop_running(&exe, r);
    copy_self(&exe)?;
    r.line(format!("Companion: {}", exe.display()));

    for sim in &sims {
        r.line(format!("MSFS 2024 ({}):", sim.label));
        match (&sim.community, EFB_PACKAGE.is_empty()) {
            (_, true) => r.line("  EFB app: not included in this build, skipped"),
            (Some(community), false) => {
                let target = install_package(community)?;
                r.line(format!("  EFB app: {}", target.display()));
            }
            (None, false) => r.line("  EFB app: Community folder not found in UserCfg.opt, skipped"),
        }
        let xml = exe_xml_path(&sim.config_dir);
        edit_exe_xml(&xml, true, |t| upsert_entry(t, &exe))?;
        r.line(format!("  Auto-start: {}", xml.display()));
    }

    if !win::find_processes(SIM_PROCESS).is_empty() {
        start_detached(&exe)?;
        r.line("The sim is running: companion started.");
    } else {
        r.line("The companion will start with the sim.");
    }
    r.line("Done.");
    Ok(())
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
        let xml = exe_xml_path(&sim.config_dir);
        match edit_exe_xml(&xml, false, |t| {
            remove_entries(t);
            Ok(())
        }) {
            Ok(true) => r.line(format!("Removed auto-start from {}", xml.display())),
            Ok(false) => {}
            Err(e) => errors.push(format!("{}: {e:#}", xml.display())),
        }
        if let Some(target) = sim.community.map(|c| c.join(PACKAGE_NAME)) {
            if target.exists() {
                match fs::remove_dir_all(&target) {
                    Ok(()) => r.line(format!("Removed {}", target.display())),
                    Err(e) => errors.push(format!("{}: {e}", target.display())),
                }
            }
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
