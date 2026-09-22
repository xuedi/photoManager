//! Fetching the GeoNames dumps. The only thing in this application that talks to the network,
//! and it only runs because someone pressed a button.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::import::SOURCE;
use super::{Error, Result};

/// What is fetched, and the name each file is saved under.
pub const FILES: [&str; 4] = [
    "countryInfo.txt",
    "admin1CodesASCII.txt",
    "cities1000.zip",
    "shapes_simplified_low.json.zip",
];

/// Nothing we fetch is anywhere near this large; a redirect to something else would be.
const LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub file: &'static str,
    pub done: usize,
    pub of: usize,
}

/// Downloads every dump into `dir` and returns where they landed. An existing file is replaced
/// only once its replacement has arrived whole.
pub fn run(dir: &Path, progress: &dyn Fn(Step), cancel: &std::sync::atomic::AtomicBool) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut landed = Vec::new();

    for (index, file) in FILES.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Malformed("the download was stopped".to_string()));
        }
        progress(Step {
            file,
            done: index,
            of: FILES.len(),
        });
        landed.push(fetch(&format!("{SOURCE}{file}"), &dir.join(file))?);
    }
    progress(Step {
        file: FILES[FILES.len() - 1],
        done: FILES.len(),
        of: FILES.len(),
    });
    Ok(landed)
}

fn fetch(url: &str, target: &Path) -> Result<PathBuf> {
    tracing::info!(url, "fetching place data");
    let response = ureq::get(url)
        .call()
        .map_err(|error| Error::Malformed(format!("{url}: {error}")))?;

    let mut body = response.into_body().into_reader().take(LIMIT);
    let partial = target.with_extension("part");
    let mut file = std::fs::File::create(&partial)?;
    std::io::copy(&mut body, &mut file)?;
    drop(file);

    if std::fs::metadata(&partial)?.len() == 0 {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::Malformed(format!("{url} sent nothing")));
    }
    std::fs::rename(&partial, target)?;
    Ok(target.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_is_asked_for_at_geonames_and_nowhere_else() {
        for file in FILES {
            let url = format!("{SOURCE}{file}");
            assert!(
                url.starts_with("https://download.geonames.org/"),
                "{url} is not the place we fetch from"
            );
        }
    }

    #[test]
    fn a_stopped_download_never_reaches_the_network() {
        let dir = std::env::temp_dir().join("photomanager-download-stopped");
        let _ = std::fs::remove_dir_all(&dir);
        let cancel = std::sync::atomic::AtomicBool::new(true);
        assert!(matches!(run(&dir, &|_| {}, &cancel), Err(Error::Malformed(_))));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "nothing was written");
    }
}
