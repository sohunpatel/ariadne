use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const PROJECT_TEMPLATE: &str = include_str!("../templates/project.toml");
const DEVICE_CATALOG: &str = include_str!("../devices.toml");

#[derive(Debug, Deserialize)]
struct ProjectFile {
    project: Project,
    tools: Tools,
    outputs: Outputs,
}

#[derive(Debug, Deserialize)]
struct Project {
    name: String,
    top: String,
    sources: Vec<PathBuf>,
    device: String,
}

#[derive(Debug, Deserialize)]
struct Tools {
    synthesis: String,
    place_route: String,
}

#[derive(Debug, Deserialize)]
struct Outputs {
    synthesized_netlist: PathBuf,
    routed_netlist: PathBuf,
}

#[derive(Debug, Deserialize)]
struct DeviceCatalog {
    devices: BTreeMap<String, Device>,
}

#[derive(Debug, Deserialize)]
struct Device {
    family: String,
    device: String,
    package: String,
    speed_grade: String,
    synthesis_pass: String,
    place_route_args: Vec<String>,
}

struct FlowPlan {
    synthesis_tool: String,
    synthesis_script: String,
    script_path: PathBuf,
    place_route_tool: String,
    place_route_args: Vec<String>,
    source_paths: Vec<PathBuf>,
    synthesized_netlist: PathBuf,
    routed_netlist: PathBuf,
    working_directory: PathBuf,
}

fn main() -> ExitCode {
    match run_cli(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ariadne: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli(args: Vec<String>) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(usage().to_owned());
    };

    match command {
        "init" => {
            let directory = match args.get(1) {
                Some(path) if args.len() == 2 => PathBuf::from(path),
                None => env::current_dir().map_err(|error| error.to_string())?,
                _ => return Err(usage().to_owned()),
            };
            initialize(&directory)
        }
        "devices" => {
            let catalog_path = match args.as_slice() {
                [_] => None,
                [_, flag, path] if flag == "--devices" => Some(PathBuf::from(path)),
                _ => return Err(usage().to_owned()),
            };
            let catalog = read_catalog(catalog_path.as_deref())?;
            for (name, device) in catalog.devices {
                println!(
                    "{name}: {} {} {} speed {}",
                    device.family, device.device, device.package, device.speed_grade
                );
            }
            Ok(())
        }
        "run" => {
            let mut project_path = PathBuf::from("project.toml");
            let mut catalog_path = None;
            let mut dry_run = false;
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "--project" if index + 1 < args.len() => {
                        project_path = PathBuf::from(&args[index + 1]);
                        index += 2;
                    }
                    "--devices" if index + 1 < args.len() => {
                        catalog_path = Some(PathBuf::from(&args[index + 1]));
                        index += 2;
                    }
                    "--dry-run" => {
                        dry_run = true;
                        index += 1;
                    }
                    _ => return Err(usage().to_owned()),
                }
            }
            run_project(&project_path, catalog_path.as_deref(), dry_run)
        }
        "--help" | "-h" => {
            println!("{}", usage());
            Ok(())
        }
        _ => Err(usage().to_owned()),
    }
}

fn usage() -> &'static str {
    "Usage: ariadne init [DIRECTORY]\n       ariadne devices [--devices FILE]\n       ariadne run [--project FILE] [--devices FILE] [--dry-run]"
}

fn initialize(directory: &Path) -> Result<(), String> {
    let project_path = directory.join("project.toml");
    let catalog_path = directory.join("devices.toml");
    if project_path.exists() || catalog_path.exists() {
        return Err(format!(
            "refusing to overwrite {} or {}",
            project_path.display(),
            catalog_path.display()
        ));
    }
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    fs::write(&project_path, PROJECT_TEMPLATE).map_err(|error| error.to_string())?;
    fs::write(&catalog_path, DEVICE_CATALOG).map_err(|error| error.to_string())?;
    println!(
        "Created {} and {}",
        project_path.display(),
        catalog_path.display()
    );
    Ok(())
}

fn read_catalog(path: Option<&Path>) -> Result<DeviceCatalog, String> {
    let contents = match path {
        Some(path) => fs::read_to_string(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?,
        None => DEVICE_CATALOG.to_owned(),
    };
    toml::from_str(&contents).map_err(|error| format!("invalid device catalog: {error}"))
}

fn run_project(
    project_path: &Path,
    catalog_path: Option<&Path>,
    dry_run: bool,
) -> Result<(), String> {
    let contents = fs::read_to_string(project_path)
        .map_err(|error| format!("could not read {}: {error}", project_path.display()))?;
    let project: ProjectFile =
        toml::from_str(&contents).map_err(|error| format!("invalid project file: {error}"))?;
    let working_directory = project_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|error| format!("could not resolve project directory: {error}"))?;
    let inferred_catalog = working_directory.join("devices.toml");
    let catalog = read_catalog(catalog_path.or_else(|| {
        inferred_catalog
            .is_file()
            .then_some(inferred_catalog.as_path())
    }))?;
    let device = catalog
        .devices
        .get(&project.project.device)
        .ok_or_else(|| {
            format!(
                "unknown device profile {:?}; run `ariadne devices` to list available profiles",
                project.project.device
            )
        })?;
    let plan = make_plan(&project, device, working_directory)?;
    execute_plan(plan, dry_run)
}

fn make_plan(
    project: &ProjectFile,
    device: &Device,
    working_directory: PathBuf,
) -> Result<FlowPlan, String> {
    if project.project.sources.is_empty() {
        return Err("project.sources must contain at least one HDL file".to_owned());
    }
    if !is_identifier(&project.project.top) || !is_identifier(&device.synthesis_pass) {
        return Err("project top and device synthesis_pass must be valid identifiers".to_owned());
    }

    let source_paths: Vec<_> = project
        .project
        .sources
        .iter()
        .map(|path| absolute_path(&working_directory, path))
        .collect();
    let synthesized_netlist =
        absolute_path(&working_directory, &project.outputs.synthesized_netlist);
    let routed_netlist = absolute_path(&working_directory, &project.outputs.routed_netlist);
    let script_path = working_directory.join("build/ariadne.ys");

    let mut script = String::new();
    for source in &source_paths {
        let extension = source.extension().and_then(|value| value.to_str());
        let flag = if extension == Some("sv") { " -sv" } else { "" };
        script.push_str(&format!("read_verilog{flag} {}\n", yosys_quote(source)));
    }
    script.push_str(&format!(
        "{} -top {} -json {}\n",
        device.synthesis_pass,
        project.project.top,
        yosys_quote(&synthesized_netlist)
    ));

    let mut place_route_args: Vec<_> = device
        .place_route_args
        .iter()
        .map(|arg| {
            arg.replace("{family}", &device.family)
                .replace("{device}", &device.device)
                .replace("{package}", &device.package)
                .replace("{speed_grade}", &device.speed_grade)
        })
        .collect();
    place_route_args.extend([
        "--json".to_owned(),
        synthesized_netlist.to_string_lossy().into_owned(),
        "--write".to_owned(),
        routed_netlist.to_string_lossy().into_owned(),
    ]);

    Ok(FlowPlan {
        synthesis_tool: project.tools.synthesis.clone(),
        synthesis_script: script,
        script_path,
        place_route_tool: project.tools.place_route.clone(),
        place_route_args,
        source_paths,
        synthesized_netlist,
        routed_netlist,
        working_directory,
    })
}

fn execute_plan(plan: FlowPlan, dry_run: bool) -> Result<(), String> {
    println!("Project directory: {}", plan.working_directory.display());
    println!(
        "Synthesis: {} -s {}",
        plan.synthesis_tool,
        plan.script_path.display()
    );
    println!(
        "Place and route: {} {}",
        plan.place_route_tool,
        plan.place_route_args.join(" ")
    );
    println!("Routed netlist: {}", plan.routed_netlist.display());
    if dry_run {
        return Ok(());
    }

    for source in &plan.source_paths {
        if !source.is_file() {
            return Err(format!("HDL source does not exist: {}", source.display()));
        }
    }
    if let Some(parent) = plan.script_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if let Some(parent) = plan.synthesized_netlist.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if let Some(parent) = plan.routed_netlist.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(&plan.script_path, plan.synthesis_script).map_err(|error| error.to_string())?;

    let synthesis_status = Command::new(&plan.synthesis_tool)
        .arg("-s")
        .arg(&plan.script_path)
        .current_dir(&plan.working_directory)
        .status()
        .map_err(|error| format!("could not start synthesis tool: {error}"))?;
    if !synthesis_status.success() {
        return Err(format!("synthesis failed with status {synthesis_status}"));
    }

    let pnr_status = Command::new(&plan.place_route_tool)
        .args(&plan.place_route_args)
        .current_dir(&plan.working_directory)
        .status()
        .map_err(|error| format!("could not start place-and-route tool: {error}"))?;
    if !pnr_status.success() {
        return Err(format!("place-and-route failed with status {pnr_status}"));
    }
    Ok(())
}

fn absolute_path(directory: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        directory.join(path)
    }
}

fn yosys_quote(path: &Path) -> String {
    let path = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{path}\"")
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_project_and_device_catalog() {
        let project: ProjectFile = toml::from_str(PROJECT_TEMPLATE).unwrap();
        let catalog = read_catalog(None).unwrap();

        assert_eq!(project.project.name, "my-fpga-project");
        assert!(catalog.devices.contains_key(&project.project.device));
    }

    #[test]
    fn builds_synthesis_and_place_route_commands_from_device_profile() {
        let project: ProjectFile = toml::from_str(PROJECT_TEMPLATE).unwrap();
        let catalog = read_catalog(None).unwrap();
        let device = catalog.devices.get(&project.project.device).unwrap();
        let plan = make_plan(&project, device, PathBuf::from("/tmp/example")).unwrap();

        assert!(plan.synthesis_script.contains("synth_ice40 -top top"));
        assert!(plan.synthesis_script.contains("/tmp/example/src/top.v"));
        assert!(plan.place_route_args.contains(&"--up5k".to_owned()));
        assert!(plan.place_route_args.contains(&"--package".to_owned()));
        assert!(plan
            .place_route_args
            .contains(&"/tmp/example/build/routed.json".to_owned()));
    }

    #[test]
    fn rejects_invalid_top_name_and_empty_sources() {
        let mut project: ProjectFile = toml::from_str(PROJECT_TEMPLATE).unwrap();
        let catalog = read_catalog(None).unwrap();
        let device = catalog.devices.get(&project.project.device).unwrap();

        project.project.top = "top;write_verilog".to_owned();
        assert!(make_plan(&project, device, PathBuf::from(".")).is_err());

        project.project.top = "top".to_owned();
        project.project.sources.clear();
        assert!(make_plan(&project, device, PathBuf::from(".")).is_err());
    }
}
