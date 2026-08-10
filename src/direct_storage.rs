use std::{env, ffi::OsString, fmt, path::PathBuf};

use crate::PROVIDER_SLUG;

/// Storage roots used only by direct, human-facing provider commands.
#[derive(Debug, PartialEq, Eq)]
pub struct DirectStoragePaths {
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootKind {
    Data,
    Cache,
}

/// A platform-standard root could not be resolved safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectStorageError {
    kind: RootKind,
}

impl fmt::Display for DirectStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (name, flag) = match self.kind {
            RootKind::Data => ("data", "--data-dir"),
            RootKind::Cache => ("cache", "--cache-dir"),
        };
        write!(
            formatter,
            "could not determine a platform-standard {name} directory; pass {flag} explicitly"
        )
    }
}

#[derive(Clone, Copy)]
enum Platform {
    #[cfg(any(test, windows))]
    Windows,
    #[cfg(any(target_os = "macos", all(test, unix)))]
    MacOs,
    #[cfg(any(test, all(unix, not(target_os = "macos"))))]
    Unix,
}

/// Resolve optional direct-CLI overrides without consulting the environment for
/// either root that the caller supplied explicitly.
pub fn resolve_direct_storage(
    data_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
) -> Result<DirectStoragePaths, DirectStorageError> {
    resolve_with(data_dir, cache_dir, current_platform(), |name| {
        env::var_os(name)
    })
}

fn resolve_with(
    data_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    platform: Platform,
    environment: impl Fn(&str) -> Option<OsString>,
) -> Result<DirectStoragePaths, DirectStorageError> {
    let data_dir = match data_dir {
        Some(path) => path,
        None => default_root(platform, RootKind::Data, &environment)?,
    };
    let cache_dir = match cache_dir {
        Some(path) => path,
        None => default_root(platform, RootKind::Cache, &environment)?,
    };
    Ok(DirectStoragePaths {
        data_dir,
        cache_dir,
    })
}

fn default_root(
    platform: Platform,
    kind: RootKind,
    environment: &impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, DirectStorageError> {
    let unavailable = || DirectStorageError { kind };
    let path = match platform {
        #[cfg(any(test, windows))]
        Platform::Windows => {
            let root = required_absolute(environment("LOCALAPPDATA")).ok_or_else(unavailable)?;
            let provider = root.join("UtterPipe").join("providers").join(PROVIDER_SLUG);
            match kind {
                RootKind::Data => provider.join("data"),
                RootKind::Cache => provider.join("cache"),
            }
        }
        #[cfg(any(target_os = "macos", all(test, unix)))]
        Platform::MacOs => {
            let home = required_absolute(environment("HOME")).ok_or_else(unavailable)?;
            match kind {
                RootKind::Data => home
                    .join("Library")
                    .join("Application Support")
                    .join("UtterPipe")
                    .join("providers")
                    .join(PROVIDER_SLUG)
                    .join("data"),
                RootKind::Cache => home
                    .join("Library")
                    .join("Caches")
                    .join("UtterPipe")
                    .join("providers")
                    .join(PROVIDER_SLUG),
            }
        }
        #[cfg(any(test, all(unix, not(target_os = "macos"))))]
        Platform::Unix => {
            let (xdg_name, home_suffix) = match kind {
                RootKind::Data => ("XDG_DATA_HOME", PathBuf::from(".local").join("share")),
                RootKind::Cache => ("XDG_CACHE_HOME", PathBuf::from(".cache")),
            };
            let root = match environment(xdg_name) {
                Some(value) => required_absolute(Some(value)).ok_or_else(unavailable)?,
                None => required_absolute(environment("HOME"))
                    .ok_or_else(unavailable)?
                    .join(home_suffix),
            };
            root.join("utterpipe").join("providers").join(PROVIDER_SLUG)
        }
    };
    Ok(path)
}

fn required_absolute(value: Option<OsString>) -> Option<PathBuf> {
    let value = value.filter(|value| !value.is_empty())?;
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

#[cfg(windows)]
const fn current_platform() -> Platform {
    Platform::Windows
}

#[cfg(target_os = "macos")]
const fn current_platform() -> Platform {
    Platform::MacOs
}

#[cfg(all(unix, not(target_os = "macos")))]
const fn current_platform() -> Platform {
    Platform::Unix
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::Path};

    use super::*;

    fn environment(values: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let values = values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
            .collect::<HashMap<_, _>>();
        move |name| values.get(name).cloned()
    }

    #[test]
    #[cfg(unix)]
    fn linux_uses_absolute_xdg_roots() {
        let paths = resolve_with(
            None,
            None,
            Platform::Unix,
            environment(&[
                ("HOME", "/home/test"),
                ("XDG_DATA_HOME", "/data"),
                ("XDG_CACHE_HOME", "/cache"),
            ]),
        )
        .unwrap();
        assert_eq!(
            paths.data_dir,
            Path::new("/data/utterpipe/providers/pocket-tts")
        );
        assert_eq!(
            paths.cache_dir,
            Path::new("/cache/utterpipe/providers/pocket-tts")
        );
    }

    #[test]
    #[cfg(unix)]
    fn linux_falls_back_to_home_when_xdg_roots_are_unset() {
        let paths = resolve_with(
            None,
            None,
            Platform::Unix,
            environment(&[("HOME", "/home/test")]),
        )
        .unwrap();
        assert_eq!(
            paths.data_dir,
            Path::new("/home/test/.local/share/utterpipe/providers/pocket-tts")
        );
        assert_eq!(
            paths.cache_dir,
            Path::new("/home/test/.cache/utterpipe/providers/pocket-tts")
        );
    }

    #[test]
    #[cfg(unix)]
    fn macos_uses_application_support_and_cache_roots() {
        let paths = resolve_with(
            None,
            None,
            Platform::MacOs,
            environment(&[("HOME", "/Users/test")]),
        )
        .unwrap();
        assert_eq!(
            paths.data_dir,
            Path::new(
                "/Users/test/Library/Application Support/UtterPipe/providers/pocket-tts/data"
            )
        );
        assert_eq!(
            paths.cache_dir,
            Path::new("/Users/test/Library/Caches/UtterPipe/providers/pocket-tts")
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_keeps_data_and_cache_as_siblings() {
        let paths = resolve_with(
            None,
            None,
            Platform::Windows,
            environment(&[("LOCALAPPDATA", r"C:\Users\test\AppData\Local")]),
        )
        .unwrap();
        let provider = Path::new(r"C:\Users\test\AppData\Local\UtterPipe\providers\pocket-tts");
        assert_eq!(paths.data_dir, provider.join("data"));
        assert_eq!(paths.cache_dir, provider.join("cache"));
    }

    #[test]
    fn explicit_roots_do_not_require_platform_environment() {
        let paths = resolve_with(
            Some(PathBuf::from("/explicit/data")),
            Some(PathBuf::from("/explicit/cache")),
            Platform::Unix,
            environment(&[]),
        )
        .unwrap();
        assert_eq!(paths.data_dir, Path::new("/explicit/data"));
        assert_eq!(paths.cache_dir, Path::new("/explicit/cache"));
    }

    #[test]
    #[cfg(unix)]
    fn one_explicit_root_only_resolves_the_missing_root() {
        let paths = resolve_with(
            Some(PathBuf::from("/explicit/data")),
            None,
            Platform::Unix,
            environment(&[("XDG_CACHE_HOME", "/cache")]),
        )
        .unwrap();
        assert_eq!(paths.data_dir, Path::new("/explicit/data"));
        assert_eq!(
            paths.cache_dir,
            Path::new("/cache/utterpipe/providers/pocket-tts")
        );
    }

    #[test]
    fn relative_or_empty_environment_roots_fail_closed() {
        let relative = resolve_with(
            None,
            Some(PathBuf::from("/explicit/cache")),
            Platform::Unix,
            environment(&[("XDG_DATA_HOME", "relative")]),
        )
        .unwrap_err();
        assert_eq!(
            relative.to_string(),
            "could not determine a platform-standard data directory; pass --data-dir explicitly"
        );

        let empty = resolve_with(
            Some(PathBuf::from("/explicit/data")),
            None,
            Platform::Windows,
            environment(&[("LOCALAPPDATA", "")]),
        )
        .unwrap_err();
        assert_eq!(
            empty.to_string(),
            "could not determine a platform-standard cache directory; pass --cache-dir explicitly"
        );
    }
}
