//! Claude 桌面端(Code tab)给每个会话记的一份 `local_<uuid>.json`:
//! `<userData>/claude-code-sessions/<账号>/<组织>/local_<uuid>.json`。一个桌面会话名下是一串
//! Claude Code 会话(当前的 `cliSessionId`,/clear、取消归档之后留下的旧 id),它们的转录照常
//! 在 `~/.claude/projects` 里。Wake 只读、只借这几样:归档位 `isArchived`、派生关系
//! `spawnedFrom` 与标题 `title`(Claude Code 的转录里没有前两样,标题只写进当时那一条:在桌面端
//! 归档了、侧栏里嵌在别的会话下面、改了名,Wake 原先都看不出来),以及"哪个桌面会话拥有这条
//! CLI 会话"——深链 `claude://resume` 要发桌面会话自己的 id(`services::terminal` 的 macOS 一支)

use super::parse_utils::{stat_stamp, text_at, true_at};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 桌面端新建会话时先往转录里写的占位标题(`custom-title`),之后起的名字只写进当时那一条
/// CLI 会话——/clear、resume 之前的那几条就一直叫这个。它不是标题
pub(crate) const PLACEHOLDER_TITLE: &str = "New session";

/// 桌面端记的一条 CLI 会话:归档了没有、桌面会话叫什么
#[derive(Debug)]
pub(crate) struct Owned {
    pub archived: bool,
    pub title: Option<String>,
}

/// 桌面端会话记录的根目录。只做路径推导、不探存在性
pub(crate) fn sessions_root() -> PathBuf {
    super::electron_user_data("Claude").join("claude-code-sessions")
}

/// 一份桌面会话记录里 Wake 用得上的部分。只有这几样:桌面端每次聚焦、每一轮都重写整份记录,
/// 别的字段变了对 Wake 就是没变(`DesktopIndex::changed` 按它比)
#[derive(Debug, PartialEq)]
pub(crate) struct Record {
    /// `sessionId` 去掉 `local_` 前缀
    pub stem: Option<String>,
    /// 当前的 Claude Code 会话(`cliSessionId`):从这个桌面会话派出去的会话挂在它下面
    pub current: Option<String>,
    /// 这个桌面会话名下的 Claude Code 会话 id:标量三字段 + 历史数组(多次 clear / resume 之后
    /// 旧 id 留在 priorCliSessionIds 里)
    pub cli_ids: Vec<String>,
    pub archived: bool,
    /// 桌面会话的名字(用户改的或自动起的;占位名不算)
    pub title: Option<String>,
    /// 派出这个会话的桌面会话(`spawnedFrom.sessionId` 去掉 `local_`):桌面端侧栏把它嵌在
    /// 那个会话下面
    pub spawned_from: Option<String>,
}

impl Record {
    pub fn parse(data: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(data).ok()?;
        let text = |key: &str| text_at(&v, key).map(str::to_string);
        let local = |id: Option<&str>| id?.strip_prefix("local_").map(str::to_string);
        let mut cli_ids: Vec<String> = [
            "cliSessionId",
            "unarchivedCliSessionId",
            "preClearCliSessionId",
        ]
        .into_iter()
        .filter_map(text)
        .collect();
        if let Some(prior) = v.get("priorCliSessionIds").and_then(Value::as_array) {
            cli_ids.extend(prior.iter().filter_map(Value::as_str).map(str::to_string));
        }
        Some(Self {
            stem: local(text_at(&v, "sessionId")),
            current: text("cliSessionId"),
            cli_ids,
            archived: true_at(&v, "isArchived"),
            title: text("title").filter(|t| t != PLACEHOLDER_TITLE),
            spawned_from: local(v.pointer("/spawnedFrom/sessionId").and_then(Value::as_str)),
        })
    }
}

/// 根目录下的全部会话记录(`<账号>/<组织>/local_*.json`)。根不在 = 没有;某一层列不出来是
/// "不知道",报错——归档位与派生关系据此保留上次读到的,不当成记录都没了
pub(crate) fn record_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    walk_records(root, true)
}

/// 同上,只是列不出来的那一层跳过:深链找拥有某条 CLI 会话的桌面会话,读得出的那几份里有就够
pub(crate) fn readable_record_files(root: &Path) -> Vec<PathBuf> {
    walk_records(root, false).unwrap_or_default()
}

fn walk_records(root: &Path, strict: bool) -> std::io::Result<Vec<PathBuf>> {
    // 不在(根不在,或列到一半被删了)就是没有
    let children = |dir: &Path| -> std::io::Result<Vec<PathBuf>> {
        let listed = match std::fs::read_dir(dir) {
            Ok(entries) => entries.map(|entry| entry.map(|e| e.path())).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        };
        match listed {
            Err(_) if !strict => Ok(Vec::new()),
            listed => listed,
        }
    };
    let mut files = Vec::new();
    for account in children(root)?.iter().filter(|p| p.is_dir()) {
        for org in children(account)?.iter().filter(|p| p.is_dir()) {
            files.extend(children(org)?.into_iter().filter(|p| is_record_name(p)));
        }
    }
    Ok(files)
}

fn is_record_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("local_") && name.ends_with(".json"))
}

/// 读一份记录;读不出、写到一半(桌面端随时在改它)给 None
fn read_record(path: &Path) -> Option<Record> {
    Record::parse(&std::fs::read(path).ok()?)
}

/// 每条 CLI 会话在桌面端的样子。同一条被两个桌面会话认领(少见):名下有它的都归档了才算
/// 归档,标题取还开着的那个的(都开着取路径排在后面的那份,次次一样)
fn owned_of(records: &[Arc<Record>]) -> HashMap<String, Owned> {
    let mut owned: HashMap<String, Owned> = HashMap::new();
    for record in records {
        for id in &record.cli_ids {
            let entry = owned.entry(id.clone()).or_insert(Owned {
                archived: true,
                title: None,
            });
            entry.archived &= record.archived;
            if record.title.is_some() && (entry.title.is_none() || !record.archived) {
                entry.title = record.title.clone();
            }
        }
    }
    owned
}

/// 桌面端侧栏的嵌套,落到 CLI 会话上:派出去的会话名下每一条 → 派它的桌面会话当前那一条。
/// 派它的会话记录不在了(删掉了)就不挂
fn spawn_links_of(records: &[Arc<Record>]) -> Vec<(String, String)> {
    let current: HashMap<&str, &str> = records
        .iter()
        .filter_map(|r| Some((r.stem.as_deref()?, r.current.as_deref()?)))
        .collect();
    records
        .iter()
        .filter_map(|r| Some((r, *current.get(r.spawned_from.as_deref()?)?)))
        .flat_map(|(r, parent)| {
            r.cli_ids
                .iter()
                .map(move |child| (child.clone(), parent.to_string()))
        })
        .collect()
}

type Stamped = ((i64, i64), Arc<Record>);

/// 桌面端会话记录的缓存:按每份文件的 (mtime, size) 记住读到的,没变的不重读
pub(crate) struct DesktopIndex {
    root: PathBuf,
    records: Mutex<BTreeMap<PathBuf, Stamped>>,
    /// watcher 上次按哪一版对过账(`take_change`)。与 `records` 分开记:扫描随时会刷新
    /// records,拿它当基准会把扫描先读到、watcher 还没对过账的改动当成没变
    settled: Mutex<HashMap<PathBuf, Arc<Record>>>,
    /// 刷新 records 时已经不在的记录,留到 watcher 对完账:一批里删掉好几份时,处理头一份的
    /// 重比就会刷新 records,后面几份原先名下有哪些会话就只有这里还知道
    vanished: Mutex<HashMap<PathBuf, Arc<Record>>>,
}

impl DesktopIndex {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            records: Mutex::new(BTreeMap::new()),
            settled: Mutex::new(HashMap::new()),
            vanished: Mutex::new(HashMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 桌面端记着的每条 CLI 会话:归档位与标题
    pub fn owned(&self) -> HashMap<String, Owned> {
        owned_of(&self.records())
    }

    /// 桌面端侧栏的嵌套:(派出去的 CLI 会话, 派它的 CLI 会话)
    pub fn spawn_links(&self) -> Vec<(String, String)> {
        spawn_links_of(&self.records())
    }

    /// 是不是一份桌面会话记录(`<根>/<账号>/<组织>/local_*.json`)
    pub fn is_record(&self, path: &Path) -> bool {
        is_record_name(path)
            && path.parent().and_then(Path::parent).and_then(Path::parent)
                == Some(self.root.as_path())
    }

    /// watcher 事件落在一份记录上:Wake 用得上的内容比上次对账时变了没有(只看、不记账)
    pub fn changed(&self, path: &Path) -> bool {
        self.diff(path, false).is_some()
    }

    /// 同上,变了就记账、交出变化前后名下的 CLI 会话;没变、不是记录给 None
    pub fn take_change(&self, path: &Path) -> Option<Vec<String>> {
        self.diff(path, true)
    }

    /// 没对过账的记录(Wake 刚起来)算变了;删掉了也算,交出它原先名下的
    fn diff(&self, path: &Path, settle: bool) -> Option<Vec<String>> {
        if !self.is_record(path) {
            return None;
        }
        let after = self.entry(path).map(|(_, record)| record);
        let mut settled = self.settled.lock().unwrap();
        let before = match (settled.get(path), &after) {
            (Some(before), Some(after)) if before == after => return None,
            (Some(before), _) => Some(before.clone()),
            // 删掉了、之前没对过账:扫描读到过的那份(两把锁分开拿,`records()` 是先 records
            // 后 vanished)
            (None, None) => {
                let vanished = self.vanished.lock().unwrap().get(path).cloned();
                vanished.or_else(|| {
                    self.records
                        .lock()
                        .unwrap()
                        .get(path)
                        .map(|(_, r)| r.clone())
                })
            }
            (None, Some(_)) => None,
        };
        if before.is_none() && after.is_none() {
            return None;
        }
        if settle {
            self.vanished.lock().unwrap().remove(path);
            match &after {
                Some(record) => {
                    settled.insert(path.to_path_buf(), record.clone());
                }
                // 删掉的记录对完账,缓存里那份也不留:不然下一次问还会把它当成刚删掉
                None => {
                    settled.remove(path);
                    self.records.lock().unwrap().remove(path);
                }
            }
        }
        let mut ids: Vec<String> = before
            .iter()
            .chain(&after)
            .flat_map(|r| r.cli_ids.iter().cloned())
            .collect();
        ids.sort();
        ids.dedup();
        Some(ids)
    }

    /// 全部记录(按路径排),现列现比(几十份,一份一次 stat;变了的才重读)。根列不出来用上次
    /// 读到的
    fn records(&self) -> Vec<Arc<Record>> {
        if let Ok(files) = record_files(&self.root) {
            let fresh: BTreeMap<PathBuf, Stamped> = files
                .into_iter()
                .filter_map(|path| Some((path.clone(), self.entry(&path)?)))
                .collect();
            let mut records = self.records.lock().unwrap();
            let before = std::mem::replace(&mut *records, fresh);
            let gone = before
                .into_iter()
                .filter(|(path, _)| !records.contains_key(path))
                .map(|(path, (_, record))| (path, record));
            self.vanished.lock().unwrap().extend(gone);
        }
        let records = self.records.lock().unwrap();
        records.values().map(|(_, record)| record.clone()).collect()
    }

    /// 一份记录此刻的内容:戳没变用缓存,变了重读、记进缓存。读不出(写到一半)交上次读到的、
    /// 记着旧戳(下次戳对不上还会再读);不在、没读到过给 None。文件 I/O 不持锁——扫描、
    /// watcher、打开详情可能同时问
    fn entry(&self, path: &Path) -> Option<Stamped> {
        let stamp = stat_stamp(path)?;
        let cached = self.records.lock().unwrap().get(path).cloned();
        match cached {
            Some((seen, record)) if seen == stamp => Some((seen, record)),
            cached => match read_record(path) {
                Some(record) => {
                    let entry = (stamp, Arc::new(record));
                    let mut records = self.records.lock().unwrap();
                    records.insert(path.to_path_buf(), entry.clone());
                    Some(entry)
                }
                None => cached,
            },
        }
    }
}

/// 测试用:在 `root` 下按桌面端的布局写一份记录(`sessionId` 由 stem 补上),返回路径
#[cfg(test)]
pub(crate) fn write_test_record(root: &Path, stem: &str, mut fields: Value) -> PathBuf {
    let org = root.join("acct").join("org");
    std::fs::create_dir_all(&org).unwrap();
    fields["sessionId"] = Value::from(format!("local_{stem}"));
    let path = org.join(format!("local_{stem}.json"));
    std::fs::write(&path, fields.to_string()).unwrap();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;
    use std::fs;

    fn archived(index: &DesktopIndex) -> HashSet<String> {
        index
            .owned()
            .into_iter()
            .filter(|(_, owned)| owned.archived)
            .map(|(id, _)| id)
            .collect()
    }

    fn write(root: &Path, stem: &str, archived: bool, cli: &str, prior: &[&str]) -> PathBuf {
        let fields =
            json!({ "cliSessionId": cli, "priorCliSessionIds": prior, "isArchived": archived });
        write_test_record(root, stem, fields)
    }

    fn ids(ids: &[&str]) -> Option<Vec<String>> {
        Some(ids.iter().map(|id| id.to_string()).collect())
    }

    #[test]
    fn a_record_lists_its_current_and_earlier_cli_sessions() {
        let record = Record::parse(
            br#"{"sessionId":"local_s","cliSessionId":"c2","preClearCliSessionId":"c1",
                 "priorCliSessionIds":["c0"],"isArchived":true}"#,
        )
        .unwrap();
        assert_eq!(record.stem.as_deref(), Some("s"));
        assert_eq!(record.current.as_deref(), Some("c2"));
        assert!(record.archived);
        assert_eq!(record.cli_ids, ["c2", "c1", "c0"]);
        assert!(
            Record::parse(b"{\"sessionId\":").is_none(),
            "写到一半的不算"
        );
    }

    /// 归档以桌面会话为单位,名下的 CLI 会话一起算;还有开着的桌面会话认领它就不算。
    /// 取消归档、记录删掉,下一次问就跟上
    #[test]
    fn archived_ids_follow_the_records() {
        let tmp = tempfile::tempdir().unwrap();
        let index = DesktopIndex::new(tmp.path().to_path_buf());
        assert!(archived(&index).is_empty(), "根不在 = 没有");

        let a = write(tmp.path(), "a", true, "cli-a", &["cli-a0", "shared"]);
        write(tmp.path(), "b", false, "cli-b", &["shared"]);
        assert_eq!(
            archived(&index),
            HashSet::from(["cli-a".to_string(), "cli-a0".to_string()])
        );

        write(tmp.path(), "a", false, "cli-a", &["cli-a0", "shared"]);
        assert!(archived(&index).is_empty(), "取消归档");

        write(tmp.path(), "a", true, "cli-a", &[]);
        assert!(archived(&index).contains("cli-a"));
        fs::remove_file(&a).unwrap();
        assert!(archived(&index).is_empty(), "记录删了");
    }

    /// 写到一半的记录沿用上次读到的,不当成没归档
    #[test]
    fn a_half_written_record_keeps_what_was_read_before() {
        let tmp = tempfile::tempdir().unwrap();
        let index = DesktopIndex::new(tmp.path().to_path_buf());
        let a = write(tmp.path(), "a", true, "cli-a", &[]);
        assert!(archived(&index).contains("cli-a"));
        fs::write(&a, "{\"sessionId\":\"local_a\",\"isArch").unwrap();
        assert!(archived(&index).contains("cli-a"));
        write(tmp.path(), "a", false, "cli-a", &[]);
        assert!(archived(&index).is_empty(), "写完了照常读");
    }

    /// 桌面会话的名字记到它名下的每条 CLI 会话上;占位名不算名字。同一条被两个桌面会话认领时
    /// 取还开着的那个的
    #[test]
    fn titles_reach_every_cli_session_of_the_desktop_session() {
        let tmp = tempfile::tempdir().unwrap();
        let index = DesktopIndex::new(tmp.path().to_path_buf());
        let record = |stem: &str, title: &str, archived: bool, ids: &[&str]| {
            let fields = json!({
                "cliSessionId": ids[0], "priorCliSessionIds": &ids[1..],
                "isArchived": archived, "title": title,
            });
            write_test_record(tmp.path(), stem, fields);
        };
        record("a", "图片下载", false, &["cli-a", "cli-a0"]);
        record("b", PLACEHOLDER_TITLE, false, &["cli-b"]);
        record("c", "Old name", true, &["cli-c", "cli-a0"]);
        let owned = index.owned();
        let title = |id: &str| owned[id].title.as_deref();
        assert_eq!(title("cli-a"), Some("图片下载"));
        assert_eq!(title("cli-a0"), Some("图片下载"), "开着的那个的");
        assert_eq!(title("cli-b"), None, "占位名不算");
        assert_eq!(title("cli-c"), Some("Old name"));
        assert!(!owned["cli-a0"].archived && owned["cli-c"].archived);
    }

    /// 派出去的会话名下每条 CLI 会话都挂到派它的桌面会话当前那条上;派它的记录不在就不挂
    #[test]
    fn spawned_sessions_hang_under_the_current_cli_session_of_their_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let index = DesktopIndex::new(tmp.path().to_path_buf());
        write(tmp.path(), "p", false, "cli-p", &["cli-p0"]);
        let spawned = |parent: &str| json!({ "sessionId": parent, "taskId": "task_1" });
        let child = json!({
            "cliSessionId": "cli-c", "priorCliSessionIds": ["cli-c0"],
            "spawnedFrom": spawned("local_p"),
        });
        write_test_record(tmp.path(), "c", child);
        let orphan = json!({ "cliSessionId": "cli-o", "spawnedFrom": spawned("local_gone") });
        write_test_record(tmp.path(), "o", orphan);

        let mut links = index.spawn_links();
        links.sort();
        let link = |c: &str, p: &str| (c.to_string(), p.to_string());
        assert_eq!(links, vec![link("cli-c", "cli-p"), link("cli-c0", "cli-p")]);
    }

    /// 同一批删掉两份记录:处理头一份时的重比会刷新缓存,第二份原先名下有哪些会话照样交得出来
    #[test]
    fn records_deleted_together_are_each_settled() {
        let tmp = tempfile::tempdir().unwrap();
        let index = DesktopIndex::new(tmp.path().to_path_buf());
        let a = write(tmp.path(), "a", true, "cli-a", &[]);
        let b = write(tmp.path(), "b", true, "cli-b", &[]);
        index.owned(); // 启动扫描读过,watcher 还没对过账
        fs::remove_file(&a).unwrap();
        fs::remove_file(&b).unwrap();
        assert_eq!(index.take_change(&a), ids(&["cli-a"]));
        index.owned(); // 头一份名下转录的重比
        assert_eq!(index.take_change(&b), ids(&["cli-b"]));
        assert_eq!(index.take_change(&b), None);
    }

    /// 某个账号目录读不出来:归档索引要的是"不知道"(报错),深链要的是读得出的那几份
    #[cfg(unix)]
    #[test]
    fn an_unreadable_account_fails_the_index_but_not_the_deep_link() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let a = write(tmp.path(), "a", false, "cli-a", &[]);
        let locked = tmp.path().join("other");
        fs::create_dir_all(locked.join("org")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            return; // root 读得动任何目录,这条测不了
        }
        assert!(record_files(tmp.path()).is_err());
        assert_eq!(readable_record_files(tmp.path()), vec![a]);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// watcher 的对账:没对过账的算变了;桌面端只是重写了 Wake 不看的字段(聚焦一下)不算;
    /// 归档变了、记录删了算,交出名下的 CLI 会话。只看(`changed`)不记账
    #[test]
    fn only_changes_wake_reads_reach_the_watcher() {
        let tmp = tempfile::tempdir().unwrap();
        let index = DesktopIndex::new(tmp.path().to_path_buf());
        let focus = |at: i64| {
            let fields = json!({ "cliSessionId": "cli-a", "lastFocusedAt": at });
            write_test_record(tmp.path(), "a", fields)
        };
        let a = focus(1);
        assert!(index.changed(&a));
        assert!(index.changed(&a), "只看不记账");
        assert_eq!(index.take_change(&a), ids(&["cli-a"]));
        focus(22);
        assert!(!index.changed(&a), "聚焦一下不算");
        assert_eq!(index.take_change(&a), None);

        write(tmp.path(), "a", true, "cli-a", &["cli-a0"]);
        assert_eq!(index.take_change(&a), ids(&["cli-a", "cli-a0"]));
        fs::remove_file(&a).unwrap();
        assert!(index.changed(&a));
        assert_eq!(index.take_change(&a), ids(&["cli-a", "cli-a0"]), "删掉了");
        assert_eq!(index.take_change(&a), None);

        let elsewhere = tmp.path().join("acct").join("local_x.json");
        fs::write(&elsewhere, "{}").unwrap();
        assert!(!index.changed(&elsewhere), "不在 <账号>/<组织>/ 这一层");
        assert!(!index.changed(&tmp.path().join("acct/org/scheduled-tasks.json")));
    }
}
