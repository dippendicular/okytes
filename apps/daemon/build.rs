use std::{env, fs, io, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=web");
    let root = Path::new("web");
    if !root.join("quick.html").is_file() {
        return Err("build local assets first: pnpm --dir apps/desktop build:quick".into());
    }
    let root = fs::canonicalize(root)?;
    let mut assets = Vec::new();
    collect(&root, &root, &mut assets)?;
    assets.sort();
    let mut source = String::from("const ASSETS: &[(&str, &[u8])] = &[\n");
    for (name, path) in assets {
        source.push_str(&format!("({name:?}, include_bytes!({path:?})),\n"));
    }
    source.push_str("];\n");
    fs::write(
        Path::new(&env::var("OUT_DIR")?).join("quick-assets.rs"),
        source,
    )?;
    Ok(())
}

fn collect(root: &Path, directory: &Path, assets: &mut Vec<(String, String)>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &entry.path(), assets)?;
        } else if kind.is_file() && entry.metadata()?.len() <= 2 * 1024 * 1024 {
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            assets.push((
                relative.to_string_lossy().replace('\\', "/"),
                path.to_string_lossy().into(),
            ));
        } else {
            return Err(io::Error::other("unexpected local frontend asset"));
        }
    }
    Ok(())
}
