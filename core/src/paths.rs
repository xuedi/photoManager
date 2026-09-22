use std::fmt;
use std::path::{Path, PathBuf};

use crate::APP_ID;

pub const LIBRARY_VAR: &str = "PHOTOMANAGER_LIBRARY";
/// A directory that already holds the GeoNames dumps, so nothing has to be downloaded.
pub const DUMPS_VAR: &str = "PHOTOMANAGER_GEONAMES";
const DEFAULT_LIBRARY: &str = "Nextcloud/Photos";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    cache: PathBuf,
    data: PathBuf,
    library: PathBuf,
    dumps: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathsError {
    NoHome,
    RelativeLibrary(PathBuf),
    LibraryMissing(PathBuf),
}

impl fmt::Display for PathsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathsError::NoHome => write!(f, "no HOME in the environment"),
            PathsError::RelativeLibrary(p) => write!(f, "{LIBRARY_VAR} must be an absolute path, got {}", p.display()),
            PathsError::LibraryMissing(p) => write!(f, "photo library not found at {}", p.display()),
        }
    }
}

impl std::error::Error for PathsError {}

impl Paths {
    pub fn from_env() -> Result<Paths, PathsError> {
        Paths::resolve(|key| std::env::var(key).ok(), |p| p.is_dir())
    }

    /// `library_exists` is passed in so the resolution can be tested without touching the disk.
    pub fn resolve<V, E>(var: V, library_exists: E) -> Result<Paths, PathsError>
    where
        V: Fn(&str) -> Option<String>,
        E: Fn(&Path) -> bool,
    {
        let non_empty = |key: &str| var(key).filter(|v| !v.trim().is_empty());
        let home = non_empty("HOME").map(PathBuf::from).ok_or(PathsError::NoHome)?;
        let base = |key: &str, fallback: &str| {
            non_empty(key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
        };

        let library = match non_empty(LIBRARY_VAR).map(PathBuf::from) {
            Some(p) if !p.is_absolute() => return Err(PathsError::RelativeLibrary(p)),
            Some(p) => p,
            None => home.join(DEFAULT_LIBRARY),
        };
        if cfg!(debug_assertions) && !library_exists(&library) {
            return Err(PathsError::LibraryMissing(library));
        }

        Ok(Paths {
            cache: base("XDG_CACHE_HOME", ".cache").join(APP_ID),
            data: base("XDG_DATA_HOME", ".local/share").join(APP_ID),
            library,
            dumps: non_empty(DUMPS_VAR).map(PathBuf::from).filter(|p| p.is_absolute()),
        })
    }

    pub fn library(&self) -> &Path {
        &self.library
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub fn thumbs_dir(&self) -> PathBuf {
        self.cache.join("thumbs")
    }

    /// Where the downloaded GeoNames dumps are kept.
    pub fn dumps_dir(&self) -> PathBuf {
        self.cache.join("geonames")
    }

    /// Dumps that are already on this machine, so the download can be skipped.
    pub fn local_dumps(&self) -> Option<&Path> {
        self.dumps.as_deref()
    }

    pub fn cache_db(&self) -> PathBuf {
        self.cache.join("cache.db")
    }

    pub fn app_db(&self) -> PathBuf {
        self.data.join("app.db")
    }

    pub fn geo_db(&self) -> PathBuf {
        self.data.join("geo.db")
    }

    pub fn all(&self) -> Vec<PathBuf> {
        vec![
            self.library.clone(),
            self.cache.clone(),
            self.data.clone(),
            self.thumbs_dir(),
            self.dumps_dir(),
            self.cache_db(),
            self.app_db(),
            self.geo_db(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |key: &str| map.get(key).cloned()
    }

    fn resolve(pairs: &[(&str, &str)]) -> Result<Paths, PathsError> {
        Paths::resolve(env(pairs), |_| true)
    }

    #[test]
    fn falls_back_to_the_xdg_defaults_below_home() {
        let paths = resolve(&[("HOME", "/home/someone")]).unwrap();
        assert_eq!(
            paths.cache_db(),
            PathBuf::from("/home/someone/.cache").join(APP_ID).join("cache.db")
        );
        assert_eq!(
            paths.app_db(),
            PathBuf::from("/home/someone/.local/share").join(APP_ID).join("app.db")
        );
        assert_eq!(paths.library(), Path::new("/home/someone/Nextcloud/Photos"));
    }

    #[test]
    fn honours_the_xdg_variables() {
        let paths = resolve(&[
            ("HOME", "/home/someone"),
            ("XDG_CACHE_HOME", "/tmp/c"),
            ("XDG_DATA_HOME", "/tmp/d"),
        ])
        .unwrap();
        assert_eq!(paths.cache_dir(), PathBuf::from("/tmp/c").join(APP_ID));
        assert_eq!(paths.data_dir(), PathBuf::from("/tmp/d").join(APP_ID));
    }

    #[test]
    fn ignores_empty_and_relative_xdg_variables() {
        let paths = resolve(&[
            ("HOME", "/home/someone"),
            ("XDG_CACHE_HOME", "  "),
            ("XDG_DATA_HOME", "relative/share"),
        ])
        .unwrap();
        assert_eq!(paths.cache_dir(), PathBuf::from("/home/someone/.cache").join(APP_ID));
        assert_eq!(
            paths.data_dir(),
            PathBuf::from("/home/someone/.local/share").join(APP_ID)
        );
    }

    #[test]
    fn the_library_variable_wins() {
        let paths = resolve(&[("HOME", "/home/someone"), (LIBRARY_VAR, "/tmp/fixture")]).unwrap();
        assert_eq!(paths.library(), Path::new("/tmp/fixture"));
    }

    #[test]
    fn refuses_a_relative_library() {
        let err = resolve(&[("HOME", "/home/someone"), (LIBRARY_VAR, "fixture")]).unwrap_err();
        assert_eq!(err, PathsError::RelativeLibrary(PathBuf::from("fixture")));
    }

    #[test]
    fn refuses_a_missing_library_and_never_creates_one() {
        let missing = PathBuf::from("/tmp/photomanager-does-not-exist");
        let err = Paths::resolve(
            env(&[
                ("HOME", "/home/someone"),
                (LIBRARY_VAR, "/tmp/photomanager-does-not-exist"),
            ]),
            |_| false,
        )
        .unwrap_err();
        assert_eq!(err, PathsError::LibraryMissing(missing.clone()));
        assert!(!missing.exists());
    }

    #[test]
    fn dumps_on_this_machine_are_used_instead_of_a_download() {
        let paths = resolve(&[("HOME", "/home/someone")]).unwrap();
        assert_eq!(paths.local_dumps(), None);
        assert_eq!(
            paths.dumps_dir(),
            PathBuf::from("/home/someone/.cache").join(APP_ID).join("geonames")
        );

        let given = resolve(&[("HOME", "/home/someone"), (DUMPS_VAR, "/tmp/geonames")]).unwrap();
        assert_eq!(given.local_dumps(), Some(Path::new("/tmp/geonames")));

        let relative = resolve(&[("HOME", "/home/someone"), (DUMPS_VAR, "geonames")]).unwrap();
        assert_eq!(relative.local_dumps(), None, "only an absolute path is taken");
    }

    #[test]
    fn needs_a_home() {
        assert_eq!(resolve(&[]).unwrap_err(), PathsError::NoHome);
    }

    #[test]
    fn a_redirected_environment_resolves_nothing_in_the_real_library() {
        let real_home = std::env::var("HOME").unwrap();
        let paths = resolve(&[
            ("HOME", "/tmp/pm-test-home"),
            ("XDG_CACHE_HOME", "/tmp/pm-test-home/cache"),
            ("XDG_DATA_HOME", "/tmp/pm-test-home/data"),
            (LIBRARY_VAR, "/tmp/pm-test-home/library"),
        ])
        .unwrap();
        for path in paths.all() {
            assert!(
                path.starts_with("/tmp/pm-test-home"),
                "{} escapes the test home",
                path.display()
            );
            assert!(
                !path.starts_with(&real_home),
                "{} points into the real home",
                path.display()
            );
        }
    }
}
