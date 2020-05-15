//! sync git api for fetching a diff

use super::utils;
use crate::error::Result;
use crate::{error::Error, hash};
use git2::{
    Delta, Diff, DiffDelta, DiffFormat, DiffHunk, DiffOptions, Patch,
    Repository,
};
use scopetime::scope_time;
use std::{fs, path::Path};

/// type of diff of a single line
#[derive(Copy, Clone, PartialEq, Hash, Debug)]
pub enum DiffLineType {
    /// just surrounding line, no change
    None,
    /// header of the hunk
    Header,
    /// line added
    Add,
    /// line deleted
    Delete,
}

impl Default for DiffLineType {
    fn default() -> Self {
        DiffLineType::None
    }
}

///
#[derive(Default, Clone, Hash, Debug)]
pub struct DiffLine {
    ///
    pub content: String,
    ///
    pub line_type: DiffLineType,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Hash)]
pub(crate) struct HunkHeader {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
}

impl From<DiffHunk<'_>> for HunkHeader {
    fn from(h: DiffHunk) -> Self {
        Self {
            old_start: h.old_start(),
            old_lines: h.old_lines(),
            new_start: h.new_start(),
            new_lines: h.new_lines(),
        }
    }
}

/// single diff hunk
#[derive(Default, Clone, Hash, Debug)]
pub struct Hunk {
    /// hash of the hunk header
    pub header_hash: u64,
    /// list of `DiffLine`s
    pub lines: Vec<DiffLine>,
}

/// collection of hunks, sum of all diff lines
#[derive(Default, Clone, Hash, Debug)]
pub struct FileDiff {
    /// list of hunks
    pub hunks: Vec<Hunk>,
    /// lines total summed up over hunks
    pub lines: u16,
}

pub(crate) fn get_diff_raw<'a>(
    repo: &'a Repository,
    p: &str,
    stage: bool,
    reverse: bool,
) -> Result<Diff<'a>> {
    let mut opt = DiffOptions::new();
    opt.pathspec(p);
    opt.reverse(reverse);

    let diff = if stage {
        // diff against head
        if let Ok(ref_head) = repo.head() {
            let parent = repo.find_commit(
                ref_head.target().ok_or_else(|| {
                    let name = ref_head.name().unwrap_or("??");
                    Error::Generic(
                        format!("can not find the target of symbolic references: {}", name)
                    )
                })?,
            )?;

            let tree = parent.tree()?;
            repo.diff_tree_to_index(
                Some(&tree),
                Some(&repo.index()?),
                Some(&mut opt),
            )?
        } else {
            repo.diff_tree_to_index(
                None,
                Some(&repo.index()?),
                Some(&mut opt),
            )?
        }
    } else {
        opt.include_untracked(true);
        opt.recurse_untracked_dirs(true);
        repo.diff_index_to_workdir(None, Some(&mut opt))?
    };

    Ok(diff)
}

///
pub fn get_diff(
    repo_path: &str,
    p: String,
    stage: bool,
) -> Result<FileDiff> {
    scope_time!("get_diff");

    let repo = utils::repo(repo_path)?;
    let repo_path = repo.path().parent().ok_or_else(|| {
        Error::Generic(
            "repositories located at root are not supported."
                .to_string(),
        )
    })?;
    let diff = get_diff_raw(&repo, &p, stage, false)?;

    let mut res: FileDiff = FileDiff::default();
    let mut current_lines = Vec::new();
    let mut current_hunk: Option<HunkHeader> = None;

    let mut adder = |header: &HunkHeader, lines: &Vec<DiffLine>| {
        res.hunks.push(Hunk {
            header_hash: hash(header),
            lines: lines.clone(),
        });
        res.lines += lines.len() as u16;
    };

    let mut put = |hunk: Option<DiffHunk>, line: git2::DiffLine| {
        if let Some(hunk) = hunk {
            let hunk_header = HunkHeader::from(hunk);

            match current_hunk {
                None => current_hunk = Some(hunk_header),
                Some(h) if h != hunk_header => {
                    adder(&h, &current_lines);
                    current_lines.clear();
                    current_hunk = Some(hunk_header)
                }
                _ => (),
            }

            let line_type = match line.origin() {
                'H' => DiffLineType::Header,
                '<' | '-' => DiffLineType::Delete,
                '>' | '+' => DiffLineType::Add,
                _ => DiffLineType::None,
            };

            let diff_line = DiffLine {
                content: String::from_utf8_lossy(line.content())
                    .to_string(),
                line_type,
            };

            current_lines.push(diff_line);
        }
    };

    let new_file_diff = if diff.deltas().len() == 1 {
        // it's safe to unwrap here because we check first that diff.deltas has a single element.
        let delta: DiffDelta = diff.deltas().next().unwrap();

        if delta.status() == Delta::Untracked {
            let relative_path =
                delta.new_file().path().ok_or_else(|| {
                    Error::Generic(
                        "new file path is unspecified.".to_string(),
                    )
                })?;

            let newfile_path = repo_path.join(relative_path);

            if let Some(newfile_content) =
                new_file_content(&newfile_path)
            {
                let mut patch = Patch::from_buffers(
                    &[],
                    None,
                    newfile_content.as_bytes(),
                    Some(&newfile_path),
                    None,
                )?;

                patch
                    .print(&mut |_delta, hunk:Option<DiffHunk>, line: git2::DiffLine| {
                        put(hunk,line);
                        true
                    })?;

                true
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    if !new_file_diff {
        diff.print(
            DiffFormat::Patch,
            |_, hunk, line: git2::DiffLine| {
                put(hunk, line);
                true
            },
        )?;
    }

    if !current_lines.is_empty() {
        adder(&current_hunk.unwrap(), &current_lines);
    }

    Ok(res)
}

fn new_file_content(path: &Path) -> Option<String> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            if let Ok(path) = fs::read_link(path) {
                return Some(path.to_str()?.to_string());
            }
        } else if meta.file_type().is_file() {
            if let Ok(content) = fs::read_to_string(path) {
                return Some(content);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::get_diff;
    use crate::error::Result;
    use crate::sync::{
        stage_add_file,
        status::{get_status, StatusType},
        tests::{get_statuses, repo_init, repo_init_empty},
    };
    use std::{
        fs::{self, File},
        io::Write,
        path::Path,
    };

    #[test]
    fn test_untracked_subfolder() {
        let (_td, repo) = repo_init().unwrap();
        let root = repo.path().parent().unwrap();
        let repo_path = root.as_os_str().to_str().unwrap();

        let res =
            get_status(repo_path, StatusType::WorkingDir).unwrap();
        assert_eq!(res.len(), 0);

        fs::create_dir(&root.join("foo")).unwrap();
        File::create(&root.join("foo/bar.txt"))
            .unwrap()
            .write_all(b"test\nfoo")
            .unwrap();

        let res =
            get_status(repo_path, StatusType::WorkingDir).unwrap();
        assert_eq!(res.len(), 1);

        let diff =
            get_diff(repo_path, "foo/bar.txt".to_string(), false)
                .unwrap();

        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].lines[1].content, "test\n");
    }

    #[test]
    fn test_empty_repo() {
        let file_path = Path::new("foo.txt");
        let (_td, repo) = repo_init_empty().unwrap();
        let root = repo.path().parent().unwrap();
        let repo_path = root.as_os_str().to_str().unwrap();

        assert_eq!(get_statuses(repo_path).unwrap(), (0, 0));

        File::create(&root.join(file_path))
            .unwrap()
            .write_all(b"test\nfoo")
            .unwrap();

        assert_eq!(get_statuses(repo_path).unwrap(), (1, 0));

        assert_eq!(
            stage_add_file(repo_path, file_path).unwrap(),
            true
        );

        assert_eq!(get_statuses(repo_path).unwrap(), (0, 1));

        let diff = get_diff(
            repo_path,
            String::from(file_path.to_str().unwrap()),
            true,
        )
        .unwrap();

        assert_eq!(diff.hunks.len(), 1);
    }

    static HUNK_A: &str = r"
1   start
2
3
4
5
6   middle
7
8
9
0
1   end";

    static HUNK_B: &str = r"
1   start
2   newa
3
4
5
6   middle
7
8
9
0   newb
1   end";

    #[test]
    fn test_hunks() {
        let (_td, repo) = repo_init().unwrap();
        let root = repo.path().parent().unwrap();
        let repo_path = root.as_os_str().to_str().unwrap();

        assert_eq!(get_statuses(repo_path).unwrap(), (0, 0));

        let file_path = root.join("bar.txt");

        {
            File::create(&file_path)
                .unwrap()
                .write_all(HUNK_A.as_bytes())
                .unwrap();
        }

        let res =
            get_status(repo_path, StatusType::WorkingDir).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].path, "bar.txt");

        let res =
            stage_add_file(repo_path, Path::new("bar.txt")).unwrap();
        assert_eq!(res, true);
        assert_eq!(get_statuses(repo_path).unwrap(), (0, 1));

        // overwrite with next content
        {
            File::create(&file_path)
                .unwrap()
                .write_all(HUNK_B.as_bytes())
                .unwrap();
        }

        assert_eq!(get_statuses(repo_path).unwrap(), (1, 1));

        let res = get_diff(repo_path, "bar.txt".to_string(), false)
            .unwrap();

        assert_eq!(res.hunks.len(), 2)
    }

    #[test]
    fn test_diff_newfile_in_sub_dir_current_dir() {
        let file_path = Path::new("foo/foo.txt");
        let (_td, repo) = repo_init_empty().unwrap();
        let root = repo.path().parent().unwrap();

        let sub_path = root.join("foo/");

        fs::create_dir_all(&sub_path).unwrap();
        File::create(&root.join(file_path))
            .unwrap()
            .write_all(b"test")
            .unwrap();

        let diff = get_diff(
            sub_path.to_str().unwrap(),
            String::from(file_path.to_str().unwrap()),
            false,
        )
        .unwrap();

        assert_eq!(diff.hunks[0].lines[1].content, "test");
    }

    #[test]
    fn test_diff_new_binary_file_using_invalid_utf8() -> Result<()> {
        let file_path = Path::new("bar");
        let (_td, repo) = repo_init_empty().unwrap();
        let root = repo.path().parent().unwrap();
        let repo_path = root.as_os_str().to_str().unwrap();

        File::create(&root.join(file_path))?
            .write_all(b"\xc3\x28")?;

        let diff = get_diff(
            repo_path,
            String::from(file_path.to_str().unwrap()),
            false,
        )
        .unwrap();

        assert_eq!(diff.hunks.len(), 0);

        Ok(())
    }
}
