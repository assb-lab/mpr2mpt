use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Directory,
    Mpr,
    Mps,
    Mpt,
    Other,
}

#[derive(Clone)]
pub(super) struct Entry {
    pub(super) path: PathBuf,
    pub(super) label: String,
    pub(super) kind: Kind,
}

pub(super) struct Browser {
    pub(super) directory: PathBuf,
    pub(super) files: Vec<Entry>,
    pub(super) parents: Vec<Entry>,
    pub(super) cursor: usize,
    pub(super) restricted: bool,
    pub(super) show_hidden: bool,
    pub(super) filter: String,
    all_files: Vec<Entry>,
    explicit: Option<Entry>,
}

impl Browser {
    pub(super) fn open(target: &Path) -> io::Result<Self> {
        Self::open_as(target, None)
    }

    pub(super) fn open_output(target: &Path) -> io::Result<Self> {
        Self::open_as(target, Some(Kind::Mpt))
    }

    fn open_as(target: &Path, kind: Option<Kind>) -> io::Result<Self> {
        let absolute = std::path::absolute(target)?;
        let (folder, selected) = if kind.is_none() && absolute.is_dir() {
            (absolute.as_path(), None)
        } else {
            if !absolute.is_file() {
                return Err(io::Error::other(format!(
                    "Cannot open path: {}",
                    absolute.display()
                )));
            }
            let containing = absolute.parent().expect("absolute file has a parent");
            (containing, absolute.file_name())
        };
        let directory = folder.canonicalize()?;
        let explicit = selected.map(|name| Entry {
            path: directory.join(name),
            label: terminal_text(Path::new(name)),
            kind: kind
                .or_else(|| measurement_kind(&absolute))
                .unwrap_or_else(|| {
                    if super::preview::has_mpr_signature(&absolute) {
                        Kind::Mpr
                    } else {
                        Kind::Other
                    }
                }),
        });
        let mut all_files = entries(&directory, false, true)?;
        if let Some(entry) = &explicit {
            keep_explicit(&mut all_files, entry);
        }
        let show_hidden = explicit
            .as_ref()
            .is_some_and(|entry| is_hidden(&entry.path));
        let parents = directory
            .parent()
            .and_then(|parent| entries(parent, true, show_hidden).ok())
            .unwrap_or_default();
        let mut browser = Self {
            directory,
            files: Vec::new(),
            parents,
            cursor: 0,
            restricted: true,
            show_hidden,
            filter: String::new(),
            all_files,
            explicit,
        };
        browser.update_files(0);
        if let Some(path) = browser.explicit.as_ref().map(|entry| entry.path.clone()) {
            browser.focus_path(&path);
        }
        Ok(browser)
    }

    pub(super) fn selected(&self) -> Option<&Entry> {
        self.files.get(self.cursor)
    }

    pub(super) fn enter_parent(&self) -> io::Result<Option<Self>> {
        let Some(parent) = self.directory.parent() else {
            return Ok(None);
        };
        let mut browser = Self::open(parent)?;
        browser.set_visibility(self.restricted, self.show_hidden);
        browser.focus_path(&self.directory);
        Ok(Some(browser))
    }

    pub(super) fn reload(&mut self) -> io::Result<()> {
        let mut files = entries(&self.directory, false, true)?;
        if let Some(entry) = &self.explicit
            && entry.path.is_file()
        {
            keep_explicit(&mut files, entry);
        }
        self.all_files = files;
        self.update_files(self.cursor);
        Ok(())
    }

    pub(super) fn set_visibility(&mut self, restricted: bool, show_hidden: bool) {
        if self.restricted != restricted || self.show_hidden != show_hidden {
            self.restricted = restricted;
            self.show_hidden = show_hidden;
            self.parents = self
                .directory
                .parent()
                .and_then(|parent| entries(parent, restricted, show_hidden).ok())
                .unwrap_or_default();
            self.update_files(self.cursor);
        }
    }

    pub(super) fn set_filter(&mut self, query: String) {
        self.filter = query;
        self.update_files(1);
    }

    fn update_files(&mut self, fallback: usize) {
        let before = self
            .selected()
            .filter(|entry| entry.path != self.directory)
            .map(|entry| entry.path.clone());
        // With no previous file rows, prefer the first real entry if one appears.
        let fallback = if self.files.len() <= 1 { 1 } else { fallback };
        let explicit = self.explicit.as_ref().map(|entry| &entry.path);
        let query = self.filter.to_lowercase();
        let current = Entry {
            path: self.directory.clone(),
            label: ".".into(),
            kind: Kind::Directory,
        };
        let visible = self.all_files.iter().filter(|entry| {
            (!self.restricted || entry.kind != Kind::Other || explicit == Some(&entry.path))
                && (self.show_hidden || !is_hidden(&entry.path))
                && (query.is_empty() || entry.label.to_lowercase().contains(&query))
        });
        self.files = std::iter::once(current).chain(visible.cloned()).collect();
        self.cursor = fallback.min(self.files.len().saturating_sub(1));
        if let Some(path) = before {
            self.focus_path(&path);
        }
    }

    pub(super) fn focus_path(&mut self, path: &Path) {
        if let Some(index) = self.files.iter().position(|entry| entry.path == path) {
            self.cursor = index;
        }
    }

    pub(super) fn walk(&mut self, distance: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(distance)
            .min(self.files.len().saturating_sub(1));
    }
}

fn keep_explicit(files: &mut Vec<Entry>, requested: &Entry) -> usize {
    if let Some(index) = files.iter().position(|entry| entry.path == requested.path) {
        files[index].kind = requested.kind;
        index
    } else {
        // The filesystem may recognize an alias whose spelling is absent from the listing.
        files.push(requested.clone());
        files.len() - 1
    }
}

fn measurement_kind(path: &Path) -> Option<Kind> {
    let suffix = path.extension()?;
    if suffix.eq_ignore_ascii_case("mpr") {
        Some(Kind::Mpr)
    } else if suffix.eq_ignore_ascii_case("mps") {
        Some(Kind::Mps)
    } else if suffix.eq_ignore_ascii_case("mpt") {
        Some(Kind::Mpt)
    } else {
        None
    }
}

pub(super) fn terminal_text(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|ch| if ch.is_control() { '�' } else { ch })
        .collect()
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."))
}

pub(super) fn entries(path: &Path, restricted: bool, show_hidden: bool) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for entry in path.read_dir()?.filter_map(Result::ok) {
        let path = entry.path();
        if !show_hidden && is_hidden(&path) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let kind = if file_type.is_dir() || (file_type.is_symlink() && path.is_dir()) {
            Kind::Directory
        } else if file_type.is_file() || (file_type.is_symlink() && path.is_file()) {
            measurement_kind(&path).unwrap_or(Kind::Other)
        } else {
            Kind::Other
        };
        if !restricted || kind != Kind::Other {
            entries.push(Entry {
                path,
                label: terminal_text(Path::new(&entry.file_name())),
                kind,
            });
        }
    }
    entries.sort_by_cached_key(|entry| {
        (
            entry.kind != Kind::Directory,
            entry.label.to_lowercase(),
            entry.path.clone(),
        )
    });
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn focuses_requested_file_and_returns_to_the_child_folder_row() {
        let scratch = tempdir().unwrap();
        let folder = scratch.path().join("測定");
        fs::create_dir(&folder).unwrap();
        let input = folder.join("sample.MPR");
        fs::write(&input, []).unwrap();
        fs::write(folder.join("sample.txt"), []).unwrap();
        fs::write(folder.join("first.mpt"), []).unwrap();

        let browser = Browser::open(&input).unwrap();
        assert_eq!(browser.files.len(), 3);
        assert_eq!(browser.selected().unwrap().label, "sample.MPR");
        let parent = browser.enter_parent().unwrap().unwrap();
        assert_eq!(parent.selected().unwrap().label, "測定");
    }

    #[test]
    fn focuses_explicit_inputs_outside_the_default_suffix_filter() {
        let scratch = tempdir().unwrap();
        let input = scratch.path().join("measurement");
        let mut header = b"BIO-LOGIC MODULAR FILE\x1a".to_vec();
        header.resize(52, 0);
        header.extend(b"MODULE");
        fs::write(&input, header).unwrap();
        fs::write(scratch.path().join("aaa.mpr"), []).unwrap();

        let browser = Browser::open(&input).unwrap();
        let entry = browser.selected().unwrap();
        assert_eq!(entry.label, "measurement");
        assert_eq!(entry.kind, Kind::Mpr);
    }

    #[test]
    fn opens_a_hidden_file_explicitly_and_allows_hiding_it_again() {
        let scratch = tempdir().unwrap();
        let requested = scratch.path().join(".requested.mpr");
        fs::write(&requested, []).unwrap();
        fs::write(scratch.path().join("other.mpr"), []).unwrap();

        let mut browser = Browser::open(&requested).unwrap();
        assert_eq!(browser.selected().unwrap().label, ".requested.mpr");
        assert!(browser.show_hidden);
        browser.set_visibility(true, false);
        assert_eq!(browser.selected().unwrap().label, "other.mpr");
    }

    #[test]
    fn keeps_a_generated_output_selected_with_its_explicit_mpt_kind() {
        let scratch = tempdir().unwrap();
        let output = scratch.path().join("result.txt");
        fs::write(&output, []).unwrap();
        let mut browser = Browser::open_output(&output).unwrap();
        browser.reload().unwrap();
        let entry = browser.selected().unwrap();
        assert_eq!(entry.label, "result.txt");
        assert_eq!(entry.kind, Kind::Mpt);
    }

    #[test]
    fn focuses_the_requested_file_on_case_insensitive_filesystems() {
        let scratch = tempdir().unwrap();
        let input = scratch.path().join("Sample.MPR");
        fs::write(&input, []).unwrap();
        fs::write(scratch.path().join("aaa.mpr"), []).unwrap();
        let alternate = scratch.path().join("sample.mpr");
        if !alternate.is_file() {
            return;
        }
        let browser = Browser::open(&alternate).unwrap();
        assert_eq!(browser.selected().unwrap().label, "sample.mpr");
    }
}
