//! Skills 页(issue #62):各家 agent 的 skill 只读总览——哪些 skill 装在哪些 agent 里、
//! 哪些是同一份(链接)、哪些是各自的拷贝、拷贝之间有没有分叉。侧栏底部入口,整页目的地,
//! 与 Memory 页同形制:左列列表 + 右侧阅读面(位置表 + 渲染后的 SKILL.md)。数据来自
//! `wake_core::skills::scan`,不进索引库:进页与页头 Refresh 时后台现扫(几十毫秒)。
//! 只看不改:动作只有 Reveal 与 Copy path——跨 agent 装 skill 是 `npx skills add` 的事,
//! 写别家的目录破只读铁律(用户 2026-10-15 定)

use super::*;
use wake_core::adapters::strip_frontmatter;
use wake_core::skills::{
    self as core_skills, Skill, SkillCache, SkillCounts, SkillInstall, SkillOwner, SkillScan,
    SkillScope,
};

/// 侧栏导航的筛选(单选互斥,与会话 / 记忆侧栏同一模型:再点当前项回到 All)
#[derive(Debug, Clone, PartialEq, Eq)]
enum SkillNav {
    All,
    /// 有两份及以上真实拷贝的(链接不算)
    Duplicated,
    Owner(SkillOwner),
    Project(PathBuf),
}

/// 阅读面正在显示的那份 SKILL.md(后台读好、去掉 frontmatter);`content_hash` 是扫描时
/// 那份的内容指纹——重扫后变了就重读,不让新说明配旧正文(按 mtime 比会漏掉保留原
/// 修改时间的改写,Codex review 2026-10-15)
struct LoadedSkill {
    /// 这一处安装的目录(`SkillInstall::path`)
    dir: PathBuf,
    content_hash: u64,
    body: SharedString,
    /// 读失败了(正文是错误提示):不算"没变",再点 / 重扫时重读(Codex review 2026-10-15)
    failed: bool,
}

pub(super) struct SkillsState {
    scan: SkillScan,
    counts: SkillCounts,
    /// 当前筛选下列表里的 skill(`scan.skills` 的下标),交 gpui::list 虚拟化
    rows: Vec<usize>,
    list: gpui::ListState,
    nav: SkillNav,
    /// 选中的 skill(`scan.skills` 的下标;重扫后按名字找回)与阅读面显示的那处安装
    /// (`installs` 的下标)
    selected: Option<usize>,
    shown: usize,
    loaded: Option<LoadedSkill>,
    scanning: bool,
    cache: Arc<SkillCache>,
    scan_task: Option<Task<()>>,
    body_task: Option<Task<()>>,
}

impl Default for SkillsState {
    fn default() -> Self {
        Self {
            scan: SkillScan::default(),
            counts: SkillCounts::default(),
            rows: Vec::new(),
            list: gpui::ListState::new(0, gpui::ListAlignment::Top, zpx(200.)),
            nav: SkillNav::All,
            selected: None,
            shown: 0,
            loaded: None,
            scanning: false,
            cache: Arc::new(SkillCache::new()),
            scan_task: None,
            body_task: None,
        }
    }
}

impl SkillsState {
    /// 换过缩放档位:行高按旧倍数量过,按比例重量(见 Workbench::sync_zoom)
    pub(super) fn remeasure(&self) {
        self.list.remeasure();
    }

    fn selected_skill(&self) -> Option<&Skill> {
        self.scan.skills.get(self.selected?)
    }
}

/// 筛选谓词:侧栏一行对应的那组 skill
fn skill_matches(skill: &Skill, nav: &SkillNav) -> bool {
    match nav {
        SkillNav::All => true,
        SkillNav::Duplicated => skill.copies > 1,
        _ => skill.installs.iter().any(|i| install_matches(i, nav)),
    }
}

/// 这一处安装是不是筛选点名的那方 / 那个项目里的(All 与 Duplicated 不点名)
fn install_matches(install: &SkillInstall, nav: &SkillNav) -> bool {
    match nav {
        SkillNav::Owner(owner) => install.owner == *owner,
        SkillNav::Project(root) => {
            matches!(&install.scope, SkillScope::Project { root: r } if r == root)
        }
        SkillNav::All | SkillNav::Duplicated => false,
    }
}

/// 选中一个 skill 时默认显示哪处:筛选点名的那方(点了 Codex 就先看 Codex 那份),
/// 否则最近改过的那份
fn default_install(skill: &Skill, nav: &SkillNav) -> usize {
    skill
        .installs
        .iter()
        .position(|i| install_matches(i, nav))
        .unwrap_or(skill.newest)
}

/// 归属的显示名:共享目录叫 Shared(译),agent 是产品名(不译)
fn owner_label(owner: SkillOwner) -> &'static str {
    match owner {
        SkillOwner::Shared => t("Shared"),
        SkillOwner::Agent(agent) => agent.display_name(),
    }
}

/// 归属的图标:agent 是品牌图(原色),共享目录是 layers 线条图标
fn owner_icon(owner: SkillOwner, size: Pixels, dark: bool) -> AnyElement {
    match owner {
        SkillOwner::Shared => icon("icons/layers.svg")
            .with_size(size)
            .flex_shrink_0()
            .into_any_element(),
        SkillOwner::Agent(agent) => img(agent.brand_icon(dark))
            .size(size)
            .flex_shrink_0()
            .into_any_element(),
    }
}

/// 一处安装的层级文案:用户级 / 项目名 / 插件名
fn scope_label(scope: &SkillScope) -> String {
    match scope {
        SkillScope::User => t("User").to_string(),
        SkillScope::Project { root } => core_skills::project_name(root),
        SkillScope::Plugin { .. } => crate::tf!("Plugin {}", scope.plugin_name().unwrap_or("")),
    }
}

impl Workbench {
    /// 进 Skills 页(`show_page`):筛选从"全部"开始、回顶部,先拿上次的结果马上画,再重扫
    /// 一次——装了新 skill 进页就看得到。会话扫描的事件不触发重扫(skill 与会话无关)
    pub(super) fn enter_skills(&mut self, cx: &mut Context<Self>) {
        self.skills.nav = SkillNav::All;
        self.apply_skill_rows(true, cx);
        self.start_skill_scan(None, cx);
    }

    /// roster 换代(location 增删、停用):skill 目录从它推,页开着就重扫;没开着下次进页本来
    /// 就会扫
    pub(super) fn rescan_open_skills(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Skills {
            self.start_skill_scan(None, cx);
        }
    }

    /// 侧栏导航行的唯一写入点:不重扫,按已有结果重新筛、回顶部
    fn set_skill_scope(&mut self, nav: SkillNav, cx: &mut Context<Self>) {
        self.skills.nav = nav;
        self.apply_skill_rows(true, cx);
    }

    /// 侧栏 Starred 式的行:点当前项回到 All,否则切到它
    fn toggle_skill_scope(&mut self, nav: SkillNav, cx: &mut Context<Self>) {
        let next = if self.skills.nav == nav {
            SkillNav::All
        } else {
            nav
        };
        self.set_skill_scope(next, cx);
    }

    /// 页头的 Refresh:只重扫 skill(各刷各的,与 Memory 页同规矩),跑完弹 "Skills refreshed"
    fn refresh_skills(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_skill_scan(
            Some((
                window.window_handle(),
                Notification::success(t("Skills refreshed")),
            )),
            cx,
        );
    }

    /// 后台扫一遍:roster 的本地实例 + 已索引的本地项目根。新的一轮覆盖旧任务(旧结果丢弃)
    fn start_skill_scan(
        &mut self,
        done: Option<(gpui::AnyWindowHandle, Notification)>,
        cx: &mut Context<Self>,
    ) {
        self.skills.scanning = true;
        cx.notify();
        let adapters = self.adapters.clone();
        let store = self.store.clone();
        let cache = self.skills.cache.clone();
        let task = cx.background_spawn(async move {
            let projects = store.local_project_roots().unwrap_or_default();
            let scan = core_skills::scan(&adapters, &projects, &cache);
            let counts = scan.counts();
            (scan, counts)
        });
        self.skills.scan_task = Some(cx.spawn(async move |this, cx| {
            let (scan, counts) = task.await;
            this.update(cx, |this, cx| {
                let state = &mut this.skills;
                let selected = state.selected_skill().map(|s| s.name.clone());
                state.scanning = false;
                state.counts = counts;
                state.scan = scan;
                state.selected =
                    selected.and_then(|name| state.scan.skills.iter().position(|s| s.name == name));
                this.apply_skill_rows(false, cx);
            })
            .ok();
            if let Some((window, note)) = done {
                cx.update_window(window, |_, window, cx| window.push_notification(note, cx))
                    .ok();
            }
        }));
    }

    /// 按当前筛选重排列表;选中项还在就留着,否则落到第一个。重扫(`to_top` 为假)保滚动
    /// 位置、显示的那处按路径找回(文件变了重读正文);换筛选回顶部,显示的那处按新筛选
    /// 重新挑(点了 Codex 就换成 Codex 那份,Codex review 2026-10-15)
    fn apply_skill_rows(&mut self, to_top: bool, cx: &mut Context<Self>) {
        let state = &mut self.skills;
        let rows: Vec<usize> = state
            .scan
            .skills
            .iter()
            .enumerate()
            .filter(|(_, s)| skill_matches(s, &state.nav))
            .map(|(ix, _)| ix)
            .collect();
        replace_list_rows(&state.list, state.rows.len(), rows.len(), to_top);
        state.rows = rows;
        match state.selected.filter(|ix| state.rows.contains(ix)) {
            Some(ix) => {
                let shown_dir = state
                    .loaded
                    .as_ref()
                    .filter(|_| !to_top)
                    .map(|l| l.dir.clone());
                self.select_skill(ix, shown_dir, cx);
            }
            None => {
                state.selected = None;
                state.loaded = None;
                if let Some(&first) = state.rows.first() {
                    self.select_skill(first, None, cx);
                }
            }
        }
        cx.notify();
    }

    /// 选中第 `ix` 个 skill;`shown_dir` 是想继续显示的那一处(重扫后按目录找回),找不到就按
    /// 筛选给默认的那处
    fn select_skill(&mut self, ix: usize, shown_dir: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(skill) = self.skills.scan.skills.get(ix) else {
            return;
        };
        let shown = shown_dir
            .and_then(|d| skill.installs.iter().position(|i| i.path == d))
            .unwrap_or_else(|| default_install(skill, &self.skills.nav));
        self.skills.selected = Some(ix);
        self.show_install(shown, cx);
    }

    /// 阅读面换到选中 skill 的第 `ix` 处安装:后台现读它的 SKILL.md(agent 刚改过也看得到)
    fn show_install(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(install) = self
            .skills
            .selected_skill()
            .and_then(|s| s.installs.get(ix))
        else {
            return;
        };
        let dir = install.path.clone();
        let md = install.skill_md();
        let content_hash = install.content_hash;
        self.skills.shown = ix;
        let unchanged = self
            .skills
            .loaded
            .as_ref()
            .is_some_and(|l| !l.failed && l.dir == dir && l.content_hash == content_hash)
            && self.skills.body_task.is_none();
        if unchanged {
            cx.notify();
            return;
        }
        self.skills.loaded = None;
        let task = {
            cx.background_spawn(async move {
                match std::fs::read(&md) {
                    Ok(bytes) => (
                        SharedString::from(
                            strip_frontmatter(&String::from_utf8_lossy(&bytes)).to_string(),
                        ),
                        false,
                    ),
                    Err(e) => (
                        crate::tf!("Could not read this skill: {}", e.to_string()).into(),
                        true,
                    ),
                }
            })
        };
        self.skills.body_task = Some(cx.spawn(async move |this, cx| {
            let (body, failed) = task.await;
            this.update(cx, |this, cx| {
                this.skills.body_task = None;
                // 只写回仍在显示的那份,快速切换时旧任务不覆盖新选择
                let current = this
                    .skills
                    .selected_skill()
                    .and_then(|s| s.installs.get(this.skills.shown));
                if current.is_some_and(|i| i.path == dir) {
                    this.skills.loaded = Some(LoadedSkill {
                        dir,
                        content_hash,
                        body,
                        failed,
                    });
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// 与 Memory 页同一形制:左列 = 列内页头 + 列表,右侧 = 阅读面
    pub(super) fn render_skills(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let count = self.skills.rows.len();
        let subtitle: SharedString = crate::tp!("{} skill", "{} skills", count).into();
        let column: AnyElement = if self.skills.rows.is_empty() {
            div().flex_1().into_any_element()
        } else {
            library_list(&self.skills.list, Self::render_skill_row, cx)
        };
        let pane: AnyElement = match self.skills.selected_skill() {
            Some(skill) => self.render_skill_reader(skill, cx),
            None => reader_placeholder(
                self.skills.scanning,
                "icons/puzzle.svg",
                t("No skills found"),
                t("Skills are folders with a SKILL.md that agents load when a task calls for them, like ~/.claude/skills or ~/.agents/skills. They show up here once installed."),
                cx,
            ),
        };
        h_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .w(self.columns().1)
                    .h_full()
                    .flex_shrink_0()
                    .bg(theme.colors.list)
                    .child(library_header(
                        "skills-header",
                        self.skill_context_title(),
                        subtitle,
                        SPACE_LG,
                        Some(self.refresh_button(self.skills.scanning, Self::refresh_skills, cx)),
                        cx,
                    ))
                    .children(self.render_skill_errors(&theme))
                    .child(column),
            )
            .child(pane)
            .into_any_element()
    }

    /// 左列页头的标题 = 当前侧栏筛选
    fn skill_context_title(&self) -> String {
        match &self.skills.nav {
            SkillNav::All => t("All Skills").to_string(),
            SkillNav::Duplicated => t("Duplicated").to_string(),
            SkillNav::Owner(owner) => owner_label(*owner).to_string(),
            SkillNav::Project(root) => core_skills::project_name(root),
        }
    }

    /// 读不出来的位置(不是"没有"):列表上方**一行**——只有一处写出它的路径,多处写个数;
    /// 悬停看明细(最多 10 条)。逐条各占一行的话,几十个项目目录读不出就会把列表挤没
    /// (Codex review 2026-10-15)
    fn render_skill_errors(&self, theme: &gpui_component::Theme) -> Option<AnyElement> {
        const MAX_LISTED: usize = 10;
        let errors = &self.skills.scan.errors;
        let label = match errors.as_slice() {
            [] => return None,
            [only] => crate::tf!("Couldn't read {}", tilde_path(&only.path.to_string_lossy())),
            many => crate::tp!(
                "Couldn't read {} location",
                "Couldn't read {} locations",
                many.len()
            ),
        };
        let mut detail: Vec<String> = errors
            .iter()
            .take(MAX_LISTED)
            .map(|e| format!("{}: {}", tilde_path(&e.path.to_string_lossy()), e.message))
            .collect();
        if errors.len() > MAX_LISTED {
            detail.push(crate::tf!("and {} more", errors.len() - MAX_LISTED));
        }
        let detail: SharedString = detail.join("\n").into();
        Some(
            h_flex()
                .id("skill-errors")
                .flex_shrink_0()
                .mx(SPACE_LG)
                .mb(SPACE_XS)
                .gap(ICON_TEXT_GAP)
                .text_size(FONT_LABEL)
                .text_color(theme.muted_foreground)
                .child(
                    icon("icons/triangle-alert.svg")
                        .with_size(zpx(12.))
                        .flex_shrink_0(),
                )
                .child(div().min_w_0().truncate().child(label))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(detail.clone()).build(window, cx)
                })
                .into_any_element(),
        )
    }

    /// 一行(记忆行同一个行盒):Body 14 medium 名字;Caption 的说明一行(没有就不占);Label
    /// 的归属图标 + 拷贝 / 链接 / 分叉徽章 + 右对齐更新时间
    fn render_skill_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let dark = theme.mode.is_dark();
        let Some((si, skill)) = self
            .skills
            .rows
            .get(ix)
            .and_then(|&si| Some((si, self.skills.scan.skills.get(si)?)))
        else {
            return div().into_any_element();
        };
        // 同一行最多五个归属图标,其余折成 +N(会话行的品牌图同一尺寸)
        const MAX_OWNER_ICONS: usize = 5;
        let more = skill.owners.len().saturating_sub(MAX_OWNER_ICONS);
        let description = skill.description();
        let meta = h_flex()
            .gap(ICON_TEXT_GAP)
            .text_size(FONT_LABEL)
            .text_color(theme.muted_foreground)
            .children(
                skill
                    .owners
                    .iter()
                    .take(MAX_OWNER_ICONS)
                    .map(|&o| owner_icon(o, zpx(15.), dark)),
            )
            .when(more > 0, |row| {
                row.child(div().flex_shrink_0().child(format!("+{more}")))
            })
            .children(skill_badges(skill, &theme))
            .child(div().flex_1())
            .child(library_row_time(("skill-time", ix), skill.updated_at()));
        let mut children = vec![library_row_title(("skill-title", ix), &skill.name, &theme)];
        if !description.is_empty() {
            children.push(
                div()
                    .w_full()
                    .min_w_0()
                    .truncate()
                    .text_size(FONT_CAPTION)
                    .text_color(theme.muted_foreground)
                    .child(description.to_string())
                    .into_any_element(),
            );
        }
        children.push(meta.into_any_element());
        library_row(
            ("skill-row", ix),
            self.skills.selected == Some(si),
            cx.listener(move |this, _, _window, cx| this.select_skill(si, None, cx)),
            children,
            &theme,
        )
    }

    /// 阅读面:详情页同款头部(显示的那处安装:归属 + 层级、Reveal / Copy path、SKILL.md
    /// 路径与更新时间),正文先是位置表,再是去掉 frontmatter 的 SKILL.md
    fn render_skill_reader(&self, skill: &Skill, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let dark = theme.mode.is_dark();
        let shown = self.skills.shown;
        let Some(install) = skill.installs.get(shown) else {
            return div().flex_1().into_any_element();
        };
        let md_path = install.skill_md().to_string_lossy().into_owned();
        let mut lead: Vec<AnyElement> = vec![
            owner_icon(install.owner, zpx(15.), dark),
            div()
                .flex_shrink_0()
                .child(owner_label(install.owner))
                .into_any_element(),
        ];
        match &install.scope {
            SkillScope::User => {}
            SkillScope::Project { root } => lead.push(project_badge(
                "skill-project",
                &root.to_string_lossy(),
                core_skills::project_name(root),
                &theme,
            )),
            SkillScope::Plugin { .. } => lead.push(
                badge(
                    scope_label(&install.scope),
                    theme.muted,
                    theme.muted_foreground,
                )
                .into_any_element(),
            ),
        }
        let (actions, meta_rows) = file_reader_parts(&md_path, install.updated_at);
        let header = detail_header_frame(
            "skill-detail-header",
            lead,
            actions,
            skill.name.clone().into(),
            meta_rows,
            &theme,
        );
        let body: AnyElement = match &self.skills.loaded {
            Some(loaded) => markdown_body(
                format!("skill-{}", loaded.dir.to_string_lossy()).into(),
                loaded.body.clone(),
                "",
                // 相对链接(references/ 一类)按这一份 skill 的目录解析
                &install.path.to_string_lossy(),
                FONT_MSG_BODY,
                gpui::rems(0.5),
                dark,
                cx,
            )
            .into_any_element(),
            None => reader_loading(),
        };
        let content = v_flex()
            .w_full()
            .gap(SPACE_LG)
            .when(!install.description.is_empty(), |col| {
                col.child(
                    div()
                        .text_size(FONT_MSG_BODY)
                        .text_color(theme.muted_foreground)
                        .child(install.description.clone()),
                )
            })
            .child(self.render_skill_locations(skill, shown, cx))
            .child(div().h(px(1.)).w_full().bg(theme.border))
            .child(body);
        reader_pane("skill-reader", header, content, &theme)
    }

    /// 位置表:这个 skill 装在哪些地方。一行一处:归属图标 + 名字、层级、路径,右端标出
    /// 链接 / 与当前显示的那份是否一致 / 插件停用,再一个 Reveal;点一行就显示那份。
    /// 有多份拷贝时末尾一句 `npx skills add` 的提示(Wake 只看不装)
    fn render_skill_locations(&self, skill: &Skill, shown: usize, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let dark = theme.mode.is_dark();
        let shown_hash = skill.installs.get(shown).map(|i| i.content_hash);
        let heading = crate::tp!("{} location", "{} locations", skill.installs.len());
        v_flex()
            .w_full()
            .gap(SPACE_XS)
            .child(
                div()
                    .text_size(FONT_LABEL)
                    .font_medium()
                    .text_color(theme.muted_foreground)
                    .child(heading),
            )
            .children(skill.installs.iter().enumerate().map(|(ix, install)| {
                let active = ix == shown;
                let path = install.path.to_string_lossy().into_owned();
                let full_path: SharedString = path.clone().into();
                let reveal = path.clone();
                let mut tags: Vec<AnyElement> = Vec::new();
                if install.linked {
                    tags.push(
                        badge(t("link"), theme.muted, theme.muted_foreground).into_any_element(),
                    );
                }
                if let SkillScope::Plugin { enabled: false, .. } = install.scope {
                    tags.push(
                        badge(t("disabled"), theme.muted, theme.muted_foreground)
                            .into_any_element(),
                    );
                }
                if skill.differs && !active {
                    tags.push(if Some(install.content_hash) == shown_hash {
                        badge(t("same"), theme.muted, theme.muted_foreground).into_any_element()
                    } else {
                        outline_badge(t("different"), theme.muted_foreground).into_any_element()
                    });
                }
                h_flex()
                    .id(("skill-location", ix))
                    .w_full()
                    .min_w_0()
                    .h(ROW_HEIGHT)
                    .px(SPACE_SM)
                    .gap(ICON_TEXT_GAP)
                    .rounded(theme.radius)
                    .text_size(FONT_CAPTION)
                    .cursor_pointer()
                    .when(active, |row| row.bg(theme.list_active))
                    .when(!active, |row| row.hover(|style| style.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, _window, cx| this.show_install(ix, cx)))
                    .child(lead_slot(
                        LEAD_BOX,
                        owner_icon(install.owner, zpx(15.), dark),
                    ))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme.foreground)
                            .child(owner_label(install.owner)),
                    )
                    // 层级与路径都可收缩(窄栏、放大档位下行不能撑出去),层级最多 160
                    .child(
                        div()
                            .min_w_0()
                            .max_w(zpx(160.))
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(scope_label(&install.scope)),
                    )
                    .child(
                        div()
                            .id(("skill-location-path", ix))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(tilde_path(&path))
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(full_path.clone())
                                    .build(window, cx)
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap(SPACE_XS)
                            .text_size(FONT_LABEL)
                            .children(tags),
                    )
                    .child(
                        Button::new(("skill-location-reveal", ix))
                            .ghost()
                            .xsmall()
                            .rounded(RADIUS_BUTTON)
                            .icon(icon("icons/folder.svg").with_size(zpx(14.)))
                            .tooltip(reveal_in_fm())
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                terminal::reveal_in_file_manager(&reveal);
                            }),
                    )
            }))
            .when(skill.copies > 1, |col| {
                col.child(
                    div()
                        .pt(SPACE_XS)
                        .text_size(FONT_CAPTION)
                        .text_color(theme.muted_foreground)
                        .child(t(
                            "To keep one copy and link it into several agents, use npx skills add.",
                        )),
                )
            })
    }

    /// Skills 页的侧栏导航,替换会话导航:All Skills / Duplicated 两行,再接 Agents(共享
    /// 目录排第一)/ Projects 两组(`sidebar_groups`,与会话、记忆侧栏同一套组件与单选模型),
    /// 计数是 skill 数
    pub(super) fn render_skills_nav(&self, cx: &Context<Self>) -> AnyElement {
        let counts = &self.skills.counts;
        let nav = &self.skills.nav;
        let to_i64 = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        let shared_row = counts
            .owners
            .iter()
            .find(|(o, _)| *o == SkillOwner::Shared)
            .map(|&(_, n)| {
                let active = *nav == SkillNav::Owner(SkillOwner::Shared);
                let path: SharedString = format!("~/{}", core_skills::SHARED_DIR).into();
                sidebar_row(
                    "skills-shared",
                    RowLead::Icon(icon("icons/layers.svg")),
                    t("Shared"),
                    Some(to_i64(n)),
                    active,
                    RowLevel::Sub,
                    cx.listener(|this, _, _window, cx| {
                        this.toggle_skill_scope(SkillNav::Owner(SkillOwner::Shared), cx);
                    }),
                    cx,
                )
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(path.clone()).build(window, cx)
                })
                .into_any_element()
            });
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .px(SIDEBAR_EDGE)
                    .pb(SPACE_XS)
                    .gap(SPACE_XS)
                    .child(sidebar_row(
                        "skills-all",
                        RowLead::Icon(icon("icons/puzzle.svg")),
                        t("All Skills"),
                        Some(to_i64(counts.total)),
                        *nav == SkillNav::All,
                        RowLevel::Primary,
                        cx.listener(|this, _, _window, cx| {
                            this.set_skill_scope(SkillNav::All, cx);
                        }),
                        cx,
                    ))
                    // 会话侧栏 Starred 的同位:再点一次回到全部,计数为零不挂徽章
                    .child(sidebar_row(
                        "skills-duplicated",
                        RowLead::Icon(icon("icons/copy.svg")),
                        t("Duplicated"),
                        (counts.duplicated > 0).then(|| to_i64(counts.duplicated)),
                        *nav == SkillNav::Duplicated,
                        RowLevel::Primary,
                        cx.listener(|this, _, _window, cx| {
                            this.toggle_skill_scope(SkillNav::Duplicated, cx);
                        }),
                        cx,
                    )),
            )
            .child(self.sidebar_groups(
                "skills-sidebar-scroll",
                shared_row,
                counts.owners.iter().filter_map(|&(owner, n)| match owner {
                    SkillOwner::Agent(agent) => {
                        Some((agent, to_i64(n), *nav == SkillNav::Owner(owner)))
                    }
                    SkillOwner::Shared => None,
                }),
                counts.projects.iter().map(|(root, n)| {
                    let path = root.to_string_lossy().into_owned();
                    let label: SharedString = core_skills::project_name(root).into();
                    let active = matches!(nav, SkillNav::Project(sel) if sel == root);
                    (path, label, to_i64(*n), active)
                }),
                |this, next, cx| {
                    this.set_skill_scope(
                        next.map_or(SkillNav::All, |a| SkillNav::Owner(SkillOwner::Agent(a))),
                        cx,
                    )
                },
                |this, next, cx| {
                    this.set_skill_scope(
                        next.map_or(SkillNav::All, |p| SkillNav::Project(PathBuf::from(p))),
                        cx,
                    )
                },
                cx,
            ))
            .into_any_element()
    }
}

/// 列表行的徽章:插件自带、几份拷贝(两份及以上)、有链接、内容分叉。插件自带的只挂徽章、
/// 不单设侧栏入口——插件是一个包(skill 之外还有命令、子 agent、钩子、MCP),在 Skills 页的
/// 侧栏放 "Plugins" 读起来像插件是 skill 的一种(用户 2026-10-23 定)
fn skill_badges(skill: &Skill, theme: &gpui_component::Theme) -> Vec<AnyElement> {
    let mut out = Vec::with_capacity(4);
    // 比灰胶囊亮一档、又不和别的徽章撞色(用户 2026-10-23):专用的紫,淡底 + 同色字
    if skill.plugin {
        let color = crate::theme::plugin_badge_color(theme.mode.is_dark());
        out.push(badge(t("plugin"), color.opacity(0.14), color).into_any_element());
    }
    if skill.copies > 1 {
        out.push(
            badge(
                crate::tp!("{} copy", "{} copies", skill.copies),
                theme.muted,
                theme.muted_foreground,
            )
            .into_any_element(),
        );
    }
    if skill.linked {
        out.push(badge(t("linked"), theme.muted, theme.muted_foreground).into_any_element());
    }
    if skill.differs {
        out.push(outline_badge(t("differs"), theme.muted_foreground).into_any_element());
    }
    out
}
