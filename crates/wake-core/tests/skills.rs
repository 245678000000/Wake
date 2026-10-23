//! skills 只读总览(issue #62):各家 skill 文件夹的归拢、拷贝 / 链接 / 分叉判定、项目级
//! 展开、Claude Code 插件。fixture 全在各自的临时目录里现搭(合成内容),不碰真实数据,
//! 也不用 adapter_contracts 那份只读的共用假 HOME。

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use wake_core::models::AgentId;
use wake_core::skills::{
    scan_in, Skill, SkillCache, SkillHomes, SkillOwner, SkillScan, SkillScope,
};

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn skill_md(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\nBody.\n")
}

/// `<dir>/<name>/SKILL.md`
fn put_skill(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    write(&path.join("SKILL.md"), text);
    path
}

/// 默认布局下各家实例确认过的 home(与 create_adapters 构造出来的同形):skill 目录挂在它下面
fn roster(home: &Path, agents: &[AgentId]) -> Vec<(AgentId, Vec<PathBuf>)> {
    agents
        .iter()
        .map(|&agent| {
            let dir = match agent {
                AgentId::ClaudeCode => ".claude",
                AgentId::Codex => ".codex",
                AgentId::Cursor => ".cursor",
                AgentId::Kiro => ".kiro",
                AgentId::Hermes => ".hermes",
                AgentId::Gemini => ".gemini",
                other => panic!("no fixture layout for {other:?}"),
            };
            (agent, vec![home.join(dir)])
        })
        .collect()
}

fn run(home: &Path, agents: &[AgentId], projects: &[PathBuf]) -> SkillScan {
    scan_in(
        &roster(home, agents),
        &SkillHomes::at(home),
        projects,
        &SkillCache::new(),
    )
}

fn by_name(scan: &SkillScan) -> BTreeMap<&str, &Skill> {
    scan.skills.iter().map(|s| (s.name.as_str(), s)).collect()
}

fn names(scan: &SkillScan) -> Vec<&str> {
    scan.skills.iter().map(|s| s.name.as_str()).collect()
}

/// roster 里这一家的默认实例(用来调 `with_custom_root`)
fn adapter(agent: AgentId) -> Box<dyn wake_core::adapters::AgentAdapter> {
    wake_core::adapters::create_adapters()
        .into_iter()
        .find(|a| a.agent() == agent)
        .unwrap()
}

#[test]
fn identical_copies_across_agents_group_into_one_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let text = skill_md("deploy", "Ship it");
    put_skill(&home.join(".claude/skills"), "deploy", &text);
    put_skill(&home.join(".codex/skills"), "deploy", &text);
    put_skill(&home.join(".kiro/skills"), "deploy", &text);
    put_skill(
        &home.join(".claude/skills"),
        "solo",
        &skill_md("solo", "Only here"),
    );

    let scan = run(
        home,
        &[AgentId::ClaudeCode, AgentId::Codex, AgentId::Kiro],
        &[],
    );
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    let skills = by_name(&scan);
    assert_eq!(skills.len(), 2);
    let deploy = skills["deploy"];
    assert_eq!(deploy.installs.len(), 3);
    assert_eq!(deploy.copies, 3);
    assert!(!deploy.differs);
    assert!(!deploy.linked);
    assert_eq!(deploy.description(), "Ship it");
    assert_eq!(
        deploy.owners,
        vec![
            SkillOwner::Agent(AgentId::ClaudeCode),
            SkillOwner::Agent(AgentId::Codex),
            SkillOwner::Agent(AgentId::Kiro),
        ]
    );
    assert!(deploy.installs.iter().all(|i| i.scope == SkillScope::User));

    let counts = scan.counts();
    assert_eq!(counts.total, 2);
    assert_eq!(counts.duplicated, 1);
    assert_eq!(
        counts.owners,
        vec![
            (SkillOwner::Agent(AgentId::ClaudeCode), 2),
            (SkillOwner::Agent(AgentId::Codex), 1),
            (SkillOwner::Agent(AgentId::Kiro), 1),
        ]
    );
}

#[test]
fn diverged_copies_are_flagged() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    put_skill(
        &home.join(".claude/skills"),
        "lint",
        &skill_md("lint", "Old"),
    );
    put_skill(
        &home.join(".cursor/skills"),
        "lint",
        &skill_md("lint", "New"),
    );
    // 只差在 SKILL.md 以外的文件:按设计只比 SKILL.md,不算分叉
    put_skill(&home.join(".claude/skills"), "same", &skill_md("same", "x"));
    let other = put_skill(&home.join(".cursor/skills"), "same", &skill_md("same", "x"));
    write(&other.join("scripts/run.sh"), "echo different\n");

    let scan = run(home, &[AgentId::ClaudeCode, AgentId::Cursor], &[]);
    let skills = by_name(&scan);
    assert!(skills["lint"].differs);
    assert!(!skills["same"].differs);
    assert_eq!(skills["same"].copies, 2);
}

#[cfg(unix)]
#[test]
fn a_symlinked_install_is_the_same_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let shared = put_skill(
        &home.join(".agents/skills"),
        "find",
        &skill_md("find", "Find"),
    );
    fs::create_dir_all(home.join(".claude/skills")).unwrap();
    // `npx skills add` 的形态:相对链接回共享目录
    std::os::unix::fs::symlink(
        "../../.agents/skills/find",
        home.join(".claude/skills/find"),
    )
    .unwrap();
    // 整个文件夹链过去的一家:里面每一项都算链接
    fs::create_dir_all(home.join(".codex")).unwrap();
    std::os::unix::fs::symlink(home.join(".agents/skills"), home.join(".codex/skills")).unwrap();

    let scan = run(home, &[AgentId::ClaudeCode, AgentId::Codex], &[]);
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    let find = by_name(&scan)["find"];
    assert_eq!(find.installs.len(), 3);
    assert_eq!(find.copies, 1);
    assert!(find.linked);
    let linked: BTreeMap<SkillOwner, bool> =
        find.installs.iter().map(|i| (i.owner, i.linked)).collect();
    assert_eq!(linked[&SkillOwner::Shared], false);
    assert_eq!(linked[&SkillOwner::Agent(AgentId::ClaudeCode)], true);
    assert_eq!(linked[&SkillOwner::Agent(AgentId::Codex)], true);
    let real = fs::canonicalize(shared).unwrap();
    assert!(find.installs.iter().all(|i| i.real_path == real));
    assert_eq!(scan.counts().duplicated, 0, "links are not duplicates");
}

#[test]
fn entries_without_a_skill_md_are_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = home.join(".claude/skills");
    put_skill(&dir, "real", &skill_md("real", "Yes"));
    fs::create_dir_all(dir.join("empty")).unwrap();
    write(&dir.join("notes/README.md"), "not a skill\n");
    write(&dir.join("loose-file.md"), "---\nname: loose\n---\n");
    // 点开头的条目(Codex 的 .system 自带 skill)不看
    put_skill(
        &dir.join(".system"),
        "builtin",
        &skill_md("builtin", "Hidden"),
    );
    put_skill(&dir, ".hidden", &skill_md("hidden", "Hidden"));
    // 不是 Hermes 的位置不按分类目录往下找
    put_skill(
        &dir.join("category"),
        "nested",
        &skill_md("nested", "Too deep"),
    );

    let scan = run(home, &[AgentId::ClaudeCode], &[]);
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    assert_eq!(names(&scan), vec!["real"]);
}

#[cfg(unix)]
#[test]
fn only_the_top_level_skill_md_is_ever_opened() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let skill = put_skill(
        &home.join(".claude/skills"),
        "big",
        &skill_md("big", "Huge repo"),
    );
    // 一个 1.3 GB 的检出的替身:打不开的兄弟文件与目录。扫描若读它们或往里递归,就会报错
    let blob = skill.join("node_modules.bin");
    write(&blob, "x");
    fs::set_permissions(&blob, fs::Permissions::from_mode(0o000)).unwrap();
    let deep = skill.join("vendor");
    put_skill(&deep, "inner", &skill_md("inner", "Must not be listed"));
    fs::set_permissions(&deep, fs::Permissions::from_mode(0o000)).unwrap();

    let scan = run(home, &[AgentId::ClaudeCode], &[]);
    fs::set_permissions(&deep, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&blob, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    assert_eq!(names(&scan), vec!["big"]);
}

#[cfg(unix)]
#[test]
fn an_unreadable_folder_is_an_error_not_an_empty_list() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = home.join(".claude/skills");
    put_skill(&dir, "a", &skill_md("a", "A"));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
    let scan = run(home, &[AgentId::ClaudeCode], &[]);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(scan.skills.is_empty());
    assert_eq!(scan.errors.len(), 1, "{:?}", scan.errors);
}

#[test]
fn project_folders_expand_under_every_indexed_project() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let alpha = tmp.path().join("work/alpha");
    let beta = tmp.path().join("work/beta");
    let text = skill_md("review", "Review a diff");
    put_skill(&alpha.join(".claude/skills"), "review", &text);
    put_skill(&beta.join(".agents/skills"), "review", &text);
    put_skill(
        &beta.join(".codex/skills"),
        "only-beta",
        &skill_md("only-beta", "B"),
    );
    put_skill(
        &home.join(".claude/skills"),
        "global",
        &skill_md("global", "G"),
    );
    // 一个没有 skill 的项目,以及家目录本身也被索引成了项目:`~/.claude/skills` 已是
    // 用户级,不能再当项目级列一遍
    let gamma = tmp.path().join("work/gamma");
    fs::create_dir_all(&gamma).unwrap();
    let projects = vec![alpha.clone(), beta.clone(), gamma, home.clone()];

    let scan = run(&home, &[AgentId::ClaudeCode, AgentId::Codex], &projects);
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    let skills = by_name(&scan);
    let review = skills["review"];
    let scopes: Vec<(SkillOwner, &SkillScope)> = review
        .installs
        .iter()
        .map(|i| (i.owner, &i.scope))
        .collect();
    assert_eq!(
        scopes,
        vec![
            (
                SkillOwner::Shared,
                &SkillScope::Project { root: beta.clone() }
            ),
            (
                SkillOwner::Agent(AgentId::ClaudeCode),
                &SkillScope::Project {
                    root: alpha.clone()
                }
            ),
        ]
    );
    assert_eq!(skills["global"].installs.len(), 1);
    assert_eq!(skills["global"].installs[0].scope, SkillScope::User);
    assert_eq!(
        scan.counts().projects,
        vec![(alpha.clone(), 1), (beta.clone(), 2)]
    );
}

#[test]
fn frontmatter_gives_name_and_description() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = home.join(".claude/skills");
    put_skill(
        &dir,
        "folded",
        "---\nname: folded\ndescription: >\n  Use when the user\n  asks for it.\nversion: 1\n---\nbody\n",
    );
    put_skill(
        &dir,
        "quoted",
        "---\nname: \"quoted\"\ndescription: \"Has: a colon\"\n---\n",
    );
    // 没有 name 用目录名;没有 description 给空串
    put_skill(&dir, "dir-name", "---\ndescription: From the dir\n---\n");
    put_skill(&dir, "bare", "# No frontmatter at all\n");
    // frontmatter 里的名字与目录名不同:按 frontmatter 归组
    put_skill(
        &dir,
        "renamed-dir",
        &skill_md("canonical", "Named in frontmatter"),
    );

    let scan = run(home, &[AgentId::ClaudeCode], &[]);
    let skills = by_name(&scan);
    assert_eq!(
        skills["folded"].description(),
        "Use when the user asks for it."
    );
    assert_eq!(skills["quoted"].description(), "Has: a colon");
    assert_eq!(skills["dir-name"].description(), "From the dir");
    assert_eq!(skills["bare"].description(), "");
    assert!(skills["canonical"].installs[0]
        .path
        .ends_with("renamed-dir"));
    assert!(!skills.contains_key("renamed-dir"));
}

#[test]
fn hermes_category_folders_are_walked_down_to_the_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = home.join(".hermes/skills");
    put_skill(&dir, "top", &skill_md("top", "Top level"));
    write(&dir.join("mlops/DESCRIPTION.md"), "Category\n");
    put_skill(
        &dir.join("mlops"),
        "axolotl",
        &skill_md("axolotl", "Fine-tune"),
    );
    put_skill(
        &dir.join("mlops/cloud"),
        "modal",
        &skill_md("modal", "Deeper"),
    );
    // 封顶:再深一层不找
    put_skill(&dir.join("a/b/c"), "too-deep", &skill_md("too-deep", "No"));

    let scan = run(home, &[AgentId::Hermes], &[]);
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    assert_eq!(names(&scan), vec!["axolotl", "modal", "top"]);
}

#[test]
fn claude_plugin_skills_come_from_the_installed_version_only() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let plugins = home.join(".claude/plugins");
    let current = plugins.join("cache/market/tools/2.0.0");
    let stale = plugins.join("cache/market/tools/1.0.0");
    let off = plugins.join("cache/market/stripe/0.1.0");
    put_skill(
        &current.join("skills"),
        "lint",
        &skill_md("lint", "Current"),
    );
    put_skill(
        &current.join("extra"),
        "fmt",
        &skill_md("fmt", "Listed in the manifest"),
    );
    write(
        &current.join(".claude-plugin/plugin.json"),
        r#"{"name": "tools", "skills": ["./extra/", "./solo"]}"#,
    );
    // 清单直接点名一个 skill 的目录(里面就有 SKILL.md)
    write(
        &current.join("solo/SKILL.md"),
        &skill_md("solo", "A single skill dir"),
    );
    put_skill(&stale.join("skills"), "lint", &skill_md("lint", "Stale"));
    put_skill(
        &stale.join("skills"),
        "gone",
        &skill_md("gone", "Only in the stale copy"),
    );
    put_skill(
        &off.join("skills"),
        "pay",
        &skill_md("pay", "Disabled plugin"),
    );
    let index = serde_json::json!({
        "version": 2,
        "plugins": {
            "tools@market": [{"scope": "user", "installPath": current, "version": "2.0.0"}],
            "stripe@market": [{"scope": "user", "installPath": off, "version": "0.1.0"}],
        }
    });
    write(&plugins.join("installed_plugins.json"), &index.to_string());
    write(
        &home.join(".claude/settings.json"),
        r#"{"enabledPlugins": {"tools@market": true, "stripe@market": false}}"#,
    );
    // 用户级同名的 lint:与插件的 `tools:lint` 是两个 skill
    put_skill(
        &home.join(".claude/skills"),
        "lint",
        &skill_md("lint", "User"),
    );

    let scan = run(home, &[AgentId::ClaudeCode], &[]);
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    let skills = by_name(&scan);
    let names: Vec<&str> = skills.keys().copied().collect();
    assert_eq!(
        names,
        vec![
            "lint",
            "stripe:pay",
            "tools:fmt",
            "tools:lint",
            "tools:solo"
        ]
    );
    assert_eq!(skills["tools:lint"].description(), "Current");
    assert_eq!(
        skills["tools:lint"].installs[0].scope,
        SkillScope::Plugin {
            id: "tools@market".into(),
            enabled: true
        }
    );
    assert_eq!(
        skills["stripe:pay"].installs[0].scope,
        SkillScope::Plugin {
            id: "stripe@market".into(),
            enabled: false
        }
    );
    assert_eq!(skills["lint"].description(), "User");
    assert_eq!(
        (skills["stripe:pay"].plugin, skills["lint"].plugin),
        (true, false)
    );
}

#[test]
fn edits_are_picked_up_and_the_scan_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let skill = put_skill(
        &home.join(".claude/skills"),
        "doc",
        &skill_md("doc", "First"),
    );
    let snapshot = |root: &Path| -> Vec<(PathBuf, u64, Option<std::time::SystemTime>)> {
        let mut out: Vec<_> = walkdir::WalkDir::new(root)
            .into_iter()
            .map(|e| {
                let e = e.unwrap();
                // 按路径现取:Windows 上目录项自带的元数据是父目录索引里缓存的那份,NTFS 延后更新,
                // 两次快照之间目录的修改时间会自己变(CI 上偶发,像是扫描写了东西)
                let m = std::fs::symlink_metadata(e.path()).unwrap();
                (e.path().to_path_buf(), m.len(), m.modified().ok())
            })
            .collect();
        out.sort();
        out
    };
    let cache = SkillCache::new();
    let roster = roster(home, &[AgentId::ClaudeCode]);
    let homes = SkillHomes::at(home);
    let before = snapshot(home);
    let first = scan_in(&roster, &homes, &[], &cache);
    assert_eq!(snapshot(home), before, "a scan must not write anything");
    assert_eq!(first.skills[0].description(), "First");
    // 内容换了(大小也变了):缓存不能交旧的
    write(&skill.join("SKILL.md"), &skill_md("doc", "Second, longer"));
    let second = scan_in(&roster, &homes, &[], &cache);
    assert_eq!(second.skills[0].description(), "Second, longer");
    assert_ne!(
        first.skills[0].installs[0].content_hash,
        second.skills[0].installs[0].content_hash
    );
}

#[test]
fn agent_folders_outside_the_roster_still_count() {
    // 会话 location 全停用的家(roster 里没有实例):固定位置与项目级目录照样列,只有挂在
    // 实例 home 下的那份跟着消失
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let project = tmp.path().join("work/app");
    put_skill(
        &home.join(".kilo/skills"),
        "fixed",
        &skill_md("fixed", "Kilo"),
    );
    put_skill(
        &home.join(".claude/skills"),
        "rooted",
        &skill_md("rooted", "Claude"),
    );
    put_skill(
        &project.join(".claude/skills"),
        "local",
        &skill_md("local", "Project"),
    );
    let scan = run(&home, &[], &[project]);
    assert_eq!(names(&scan), vec!["fixed", "local"]);
}

#[test]
fn adapter_homes_need_structural_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    // 默认实例的构造会探 env 根与家目录(Hermes 列 profiles):先把它们指到空的假 HOME
    let fake_home = base.join("home");
    fs::create_dir_all(&fake_home).unwrap();
    common::isolate_home(&fake_home);
    // 每家都拿一个空目录当自定义根:报出来的 home 必须是数据根的上层(或就是它的目录),
    // 不能凭名字往外摸
    for adapter in wake_core::adapters::create_adapters() {
        let dir = base.join(format!("bare-{}", adapter.agent().as_str()));
        fs::create_dir_all(&dir).unwrap();
        let custom = adapter.with_custom_root(dir);
        let roots = custom.data_roots();
        for home in custom.homes() {
            assert!(
                roots.iter().all(|r| r.starts_with(&home)),
                "{:?}: home {home:?} is not above {roots:?}",
                adapter.agent()
            );
        }
    }
    // 选的是 home 形态 → home 就是它;直接选了数据目录本身 → 没有 home(不越界摸父目录)
    let cases: &[(AgentId, &str)] = &[
        (AgentId::ClaudeCode, "projects"),
        (AgentId::Codex, "sessions"),
        (AgentId::Qoder, "projects"),
        (AgentId::Cursor, "projects"),
        (AgentId::Codebuddy, "projects"),
        (AgentId::Dsh, "sessions"),
        (AgentId::Kiro, "sessions/cli"),
        (AgentId::Pi, "agent/sessions"),
        (AgentId::CraftAgents, "workspaces"),
        // 读边车时 Grok 会按名字去 sessions 的父目录找,但那不是用户选的范围
        (AgentId::Grok, "sessions"),
    ];
    for &(agent, data) in cases {
        let home = base.join(format!("home-{}", agent.as_str()));
        let data_dir = data.split('/').fold(home.clone(), |p, seg| p.join(seg));
        fs::create_dir_all(&data_dir).unwrap();
        if agent == AgentId::Codex {
            // Codex 认 home 要独立证据(state DB 或兄弟目录),见 normalize_custom_root
            fs::create_dir_all(home.join("archived_sessions")).unwrap();
        }
        let adapter = adapter(agent);
        let expected = if agent == AgentId::Pi {
            home.join("agent")
        } else {
            home.clone()
        };
        assert_eq!(
            adapter.with_custom_root(home.clone()).homes(),
            vec![expected],
            "{agent:?} home form"
        );
        let bare = adapter.with_custom_root(data_dir.clone());
        assert!(bare.homes().is_empty(), "{agent:?} bare {data_dir:?}");
    }
    // Pi / Oh My Pi:直接选了 agent 那一层(里面就有 sessions/),它自己就是 home
    let agent_dir = base.join("pi-agent");
    fs::create_dir_all(agent_dir.join("sessions")).unwrap();
    for agent in [AgentId::Pi, AgentId::Omp] {
        assert_eq!(
            adapter(agent).with_custom_root(agent_dir.clone()).homes(),
            vec![agent_dir.clone()],
            "{agent:?} agent dir"
        );
    }
    // Hermes:选目录(home 或档案目录)就是 home;直接给一份 state.db 文件则没有
    let hermes = adapter(AgentId::Hermes);
    let dir = base.join("hermes-home");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("state.db"), b"").unwrap();
    assert_eq!(
        hermes.with_custom_root(dir.clone()).homes(),
        vec![dir.clone()]
    );
    assert!(hermes
        .with_custom_root(dir.join("state.db"))
        .homes()
        .is_empty());
}

#[cfg(unix)]
#[test]
fn a_project_folder_linked_to_the_shared_one_is_still_an_install() {
    // `<repo>/.claude/skills` 链到 ~/.agents/skills:真实路径与共享目录相同,但它是项目里
    // 的一处安装,不能当成"家目录被索引成项目"那种重复丢掉
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("work/repo");
    put_skill(
        &home.join(".agents/skills"),
        "shared",
        &skill_md("shared", "S"),
    );
    fs::create_dir_all(repo.join(".claude")).unwrap();
    std::os::unix::fs::symlink(home.join(".agents/skills"), repo.join(".claude/skills")).unwrap();
    let scan = run(&home, &[AgentId::ClaudeCode], &[repo.clone(), home.clone()]);
    let shared = by_name(&scan)["shared"];
    let scopes: Vec<(SkillOwner, &SkillScope)> = shared
        .installs
        .iter()
        .map(|i| (i.owner, &i.scope))
        .collect();
    assert_eq!(
        scopes,
        vec![
            (SkillOwner::Shared, &SkillScope::User),
            (
                SkillOwner::Agent(AgentId::ClaudeCode),
                &SkillScope::Project { root: repo.clone() }
            ),
        ],
        "the home-as-project copy of ~/.agents/skills is still skipped"
    );
    assert_eq!(shared.copies, 1);
    assert!(shared.installs[1].linked);
}

#[cfg(unix)]
#[test]
fn a_linked_entry_inside_a_linked_folder_is_still_one_copy() {
    // .codex/skills 整个链到 .agents/skills,后者的 deploy 又链到外部目录:三处是同一份
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let external = put_skill(
        &tmp.path().join("vendor"),
        "deploy",
        &skill_md("deploy", "D"),
    );
    fs::create_dir_all(home.join(".agents/skills")).unwrap();
    std::os::unix::fs::symlink(&external, home.join(".agents/skills/deploy")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();
    std::os::unix::fs::symlink(home.join(".agents/skills"), home.join(".codex/skills")).unwrap();
    let scan = run(&home, &[AgentId::Codex], &[]);
    let deploy = by_name(&scan)["deploy"];
    assert_eq!(deploy.installs.len(), 2);
    assert_eq!(deploy.copies, 1);
    assert_eq!(scan.counts().duplicated, 0);
}

#[test]
fn plugin_manifest_paths_that_overlap_or_point_at_one_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let plugins = home.join(".claude/plugins");
    // a:清单列了默认 skills/ 下的一个 skill(两条读法读到同一处,只记一次)
    let a = plugins.join("cache/m/a/1.0.0");
    put_skill(&a.join("skills"), "deploy", &skill_md("deploy", "A"));
    write(
        &a.join(".claude-plugin/plugin.json"),
        r#"{"skills": ["./skills/deploy"]}"#,
    );
    // b:清单说 skills 在 ./skills,而 SKILL.md 就直接放在 skills/ 里(本身是一个 skill)
    let b = plugins.join("cache/m/b/1.0.0");
    write(&b.join("skills/SKILL.md"), &skill_md("whole", "B"));
    write(
        &b.join(".claude-plugin/plugin.json"),
        r#"{"skills": "./skills"}"#,
    );
    let index = serde_json::json!({
        "plugins": {
            "a@m": [{"installPath": a}],
            "b@m": [{"installPath": b}],
        }
    });
    write(&plugins.join("installed_plugins.json"), &index.to_string());
    let scan = run(home, &[AgentId::ClaudeCode], &[]);
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    let skills = by_name(&scan);
    assert_eq!(
        skills.keys().copied().collect::<Vec<_>>(),
        vec!["a:deploy", "b:whole"]
    );
    assert_eq!(skills["a:deploy"].installs.len(), 1);
}

#[test]
fn a_broken_plugin_manifest_is_reported_not_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let plugins = home.join(".claude/plugins");
    let install = plugins.join("cache/m/p/1.0.0");
    put_skill(
        &install.join("skills"),
        "kept",
        &skill_md("kept", "Default dir still read"),
    );
    write(&install.join(".claude-plugin/plugin.json"), "{ not json");
    let index = serde_json::json!({"plugins": {"p@m": [{"installPath": install}]}});
    write(&plugins.join("installed_plugins.json"), &index.to_string());
    let scan = run(home, &[AgentId::ClaudeCode], &[]);
    assert_eq!(scan.errors.len(), 1, "{:?}", scan.errors);
    assert!(scan.errors[0].path.ends_with("plugin.json"));
    assert!(by_name(&scan).contains_key("p:kept"));
}

#[test]
fn overlapping_plugin_paths_are_read_once_whatever_the_order() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let plugins = home.join(".claude/plugins");
    // c:清单先点名 extra 里的一个 skill、再列整个 extra——同一处只记一次
    let c = plugins.join("cache/m/c/1.0.0");
    put_skill(&c.join("extra"), "deploy", &skill_md("deploy", "C"));
    write(
        &c.join(".claude-plugin/plugin.json"),
        r#"{"skills": ["./extra/deploy", "./extra"]}"#,
    );
    // d:`skills/` 本身是一个 skill,清单又点名了它下面的 deploy——两个都在
    let d = plugins.join("cache/m/d/1.0.0");
    write(&d.join("skills/SKILL.md"), &skill_md("whole", "D"));
    put_skill(&d.join("skills"), "deploy", &skill_md("deploy", "D2"));
    write(
        &d.join(".claude-plugin/plugin.json"),
        r#"{"skills": ["./skills/deploy"]}"#,
    );
    let index = serde_json::json!({
        "plugins": {
            "c@m": [{"installPath": c}],
            "d@m": [{"installPath": d}],
        }
    });
    write(&plugins.join("installed_plugins.json"), &index.to_string());
    let scan = run(home, &[AgentId::ClaudeCode], &[]);
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    assert_eq!(names(&scan), vec!["c:deploy", "d:deploy", "d:whole"]);
    assert_eq!(by_name(&scan)["c:deploy"].installs.len(), 1);
}
