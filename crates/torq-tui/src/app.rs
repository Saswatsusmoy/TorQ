//! TUI state and key handling.
//!
//! The interaction model: a sidebar rail of sections (source
//! categories plus Downloads/Seeding), a sidebar/content region focus, and a
//! results list that opens into a detail view. Search is edited in a dedicated
//! mode (`/`) instead of typing straight into the list.

use std::sync::OnceLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use torq_core::daemon::{Status, TorrentView};
use torq_sources::{Registry, SourceGroup, TorrentResult};

use crate::net::{Action, Client, UiMsg};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Splash,
    Browser,
}

/// Sidebar sections, in display order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    All,
    Games,
    Movies,
    Tv,
    Anime,
    Downloads,
    Seeding,
}

impl Section {
    /// The source group this section filters by; `None` for All and the
    /// torrent lists.
    pub fn group(self) -> Option<SourceGroup> {
        match self {
            Section::All => None,
            Section::Games => Some(SourceGroup::Games),
            Section::Movies => Some(SourceGroup::Movies),
            Section::Tv => Some(SourceGroup::Tv),
            Section::Anime => Some(SourceGroup::Anime),
            Section::Downloads | Section::Seeding => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::All => "All",
            Section::Games => "Games",
            Section::Movies => "Movies",
            Section::Tv => "TV",
            Section::Anime => "Anime",
            Section::Downloads => "Downloads",
            Section::Seeding => "Seeding",
        }
    }

    pub const ALL: [Section; 7] = [
        Section::All,
        Section::Games,
        Section::Movies,
        Section::Tv,
        Section::Anime,
        Section::Downloads,
        Section::Seeding,
    ];

    pub fn prev(self) -> Section {
        let i = Section::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Section::ALL[(i + Section::ALL.len() - 1) % Section::ALL.len()]
    }

    pub fn next(self) -> Section {
        let i = Section::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Section::ALL[(i + 1) % Section::ALL.len()]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Region {
    Sidebar,
    Content,
}

/// Results-list interaction mode (search sections only).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchMode {
    List,
    Detail,
    /// Editing the search query in the search bar.
    Editing,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortField {
    Size,
    Seeders,
    Source,
    Added,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    None,
    SizeAsc,
    SizeDesc,
    SeedersAsc,
    SeedersDesc,
    SourceAsc,
    SourceDesc,
    AddedAsc,
    AddedDesc,
}

impl Sort {
    pub const CYCLE: [Sort; 9] = [
        Sort::None,
        Sort::SizeAsc,
        Sort::SizeDesc,
        Sort::SeedersAsc,
        Sort::SeedersDesc,
        Sort::SourceAsc,
        Sort::SourceDesc,
        Sort::AddedAsc,
        Sort::AddedDesc,
    ];

    pub fn next(self) -> Sort {
        let i = Sort::CYCLE.iter().position(|s| *s == self).unwrap_or(0);
        Sort::CYCLE[(i + 1) % Sort::CYCLE.len()]
    }

    fn field(self) -> SortField {
        match self {
            Sort::SizeAsc | Sort::SizeDesc => SortField::Size,
            Sort::SeedersAsc | Sort::SeedersDesc => SortField::Seeders,
            Sort::SourceAsc | Sort::SourceDesc => SortField::Source,
            _ => SortField::Added,
        }
    }

    /// Does this sort target `field` (for header arrows)?
    pub fn field_matches(self, field: SortField) -> bool {
        self != Sort::None && self.field() == field
    }

    fn asc(self) -> bool {
        matches!(
            self,
            Sort::SizeAsc | Sort::SeedersAsc | Sort::SourceAsc | Sort::AddedAsc
        )
    }

    /// Arrow glyph for the sort column header (`▴` asc / `▾` desc).
    pub fn arrow(self) -> Option<char> {
        match self {
            Sort::None => None,
            _ => Some(if self.asc() { '▴' } else { '▾' }),
        }
    }

    /// Short status label, e.g. `size ▾`.
    pub fn label(self) -> &'static str {
        match self {
            Sort::None => "default",
            Sort::SizeAsc => "size ▴",
            Sort::SizeDesc => "size ▾",
            Sort::SeedersAsc => "seeders ▴",
            Sort::SeedersDesc => "seeders ▾",
            Sort::SourceAsc => "source ▴",
            Sort::SourceDesc => "source ▾",
            Sort::AddedAsc => "added ▴",
            Sort::AddedDesc => "added ▾",
        }
    }
}

/// Sort a result list in place. Primary direction follows `sort`; the seeders
/// tiebreak is always descending.
pub fn sort_results(list: &mut [TorrentResult], sort: Sort) {
    if sort == Sort::None {
        return;
    }
    let field = sort.field();
    let asc = sort.asc();
    list.sort_by(|a, b| {
        let primary = match field {
            SortField::Size => a.size_bytes.cmp(&b.size_bytes),
            SortField::Seeders => a.seeders.cmp(&b.seeders),
            SortField::Source => a.source.cmp(&b.source),
            SortField::Added => a.added.unwrap_or(0).cmp(&b.added.unwrap_or(0)),
        };
        let primary = if asc { primary } else { primary.reverse() };
        primary.then_with(|| b.seeders.cmp(&a.seeders))
    });
}

/// Source id → groups, resolved once from the built-in + plugin registry.
fn groups_by_source() -> &'static std::collections::HashMap<String, Vec<SourceGroup>> {
    static GROUPS: OnceLock<std::collections::HashMap<String, Vec<SourceGroup>>> = OnceLock::new();
    GROUPS.get_or_init(|| {
        Registry::all()
            .sources
            .iter()
            .map(|s| (s.id().to_string(), s.groups().to_vec()))
            .collect()
    })
}

/// Does a result's source belong to `group`? Unknown ids (pasted magnet
/// results, removed plugins) match nothing.
pub fn in_group(source: &str, group: SourceGroup) -> bool {
    groups_by_source()
        .get(source)
        .is_some_and(|groups| groups.contains(&group))
}

/// Total configured sources (built-ins + plugins), for "all sources down".
pub fn source_count() -> usize {
    groups_by_source().len()
}

pub struct App {
    pub view: View,
    pub section: Section,
    pub region: Region,
    pub base: String,
    // Search
    pub query: String,
    /// Live text in the search bar (editing or splash).
    pub edit: String,
    pub mode: SearchMode,
    pub searching: bool,
    pub results: Vec<TorrentResult>,
    pub offline: Vec<String>,
    pub sort: Sort,
    pub cursor: usize,
    pub detail: Option<TorrentResult>,
    // Downloads / seeding
    pub torrents: Vec<TorrentView>,
    pub dl_cursor: usize,
    // Shared
    pub help: bool,
    pub notice: Option<String>,
    /// Frame counter driving the progress-bar sheen and spinner.
    pub tick: u64,
}

impl App {
    pub fn new(base: String) -> Self {
        Self {
            view: View::Splash,
            section: Section::All,
            region: Region::Content,
            base,
            query: String::new(),
            edit: String::new(),
            mode: SearchMode::List,
            searching: false,
            results: Vec::new(),
            offline: Vec::new(),
            sort: Sort::None,
            cursor: 0,
            detail: None,
            torrents: Vec::new(),
            dl_cursor: 0,
            help: false,
            notice: None,
            tick: 0,
        }
    }

    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn is_search_section(&self) -> bool {
        self.section.group().is_some() || self.section == Section::All
    }

    /// Results visible in the current section: group-filtered, then sorted.
    pub fn visible_results(&self) -> Vec<TorrentResult> {
        let mut v: Vec<TorrentResult> = match self.section.group() {
            Some(g) => self
                .results
                .iter()
                .filter(|r| in_group(&r.source, g))
                .cloned()
                .collect(),
            None => self.results.clone(),
        };
        sort_results(&mut v, self.sort);
        v
    }

    pub fn active_torrents(&self) -> Vec<&TorrentView> {
        self.torrents
            .iter()
            .filter(|t| t.status != Status::Completed)
            .collect()
    }

    pub fn seeding_torrents(&self) -> Vec<&TorrentView> {
        self.torrents
            .iter()
            .filter(|t| t.status == Status::Completed)
            .collect()
    }

    pub fn active_count(&self) -> usize {
        self.torrents
            .iter()
            .filter(|t| t.status != Status::Completed)
            .count()
    }

    pub fn seeding_count(&self) -> usize {
        self.torrents
            .iter()
            .filter(|t| t.status == Status::Completed)
            .count()
    }

    /// Torrents shown by the current section: active (non-completed) under
    /// Downloads, completed under Seeding. `dl_cursor` indexes this list.
    pub fn visible_torrents(&self) -> Vec<&TorrentView> {
        match self.section {
            Section::Seeding => self.seeding_torrents(),
            _ => self.active_torrents(),
        }
    }

    pub fn apply(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Torrents(v) => {
                self.torrents = v;
                let len = self.visible_torrents().len();
                self.dl_cursor = self.dl_cursor.min(len.saturating_sub(1));
            }
            UiMsg::Search(Ok(report)) => {
                self.results = report.results;
                self.offline = report.offline;
                self.searching = false;
                self.cursor = 0;
                self.detail = None;
            }
            UiMsg::Search(Err(e)) => {
                self.notice = Some(e.to_string());
                self.searching = false;
            }
            UiMsg::Notice(s) => self.notice = Some(s),
        }
    }

    fn submit_search(&mut self, client: &Client) {
        self.query = self.edit.clone();
        self.searching = true;
        self.notice = None;
        self.mode = SearchMode::List;
        self.cursor = 0;
        self.detail = None;
        client.send(Action::Search(self.query.clone()));
    }

    /// Start the browser view from the splash (Enter on the search bar).
    fn enter_browser(&mut self, client: &Client) {
        self.view = View::Browser;
        self.section = Section::All;
        self.region = Region::Content;
        self.submit_search(client);
    }

    fn set_section(&mut self, s: Section) {
        if self.section != s {
            self.section = s;
            self.mode = SearchMode::List;
            self.detail = None;
        }
    }

    fn move_cursor(&mut self, delta: i32) {
        let len = self.visible_results().len();
        self.cursor = if len == 0 {
            0
        } else {
            (self.cursor as i32 + delta).clamp(0, len as i32 - 1) as usize
        };
    }

    fn move_dl_cursor(&mut self, delta: i32) {
        let len = self.visible_torrents().len();
        self.dl_cursor = if len == 0 {
            0
        } else {
            (self.dl_cursor as i32 + delta).clamp(0, len as i32 - 1) as usize
        };
    }

    fn add_result(&self, r: &TorrentResult, client: &Client) {
        client.send(Action::Add {
            magnet: r.magnet.clone(),
        });
    }

    /// Handle a key; returns `None` when the user wants to quit.
    pub fn handle_key(&mut self, key: KeyEvent, client: &Client) -> Option<()> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        if self.view == View::Splash {
            return self.splash_key(key, client);
        }
        if self.help {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('?' | 'q') => {
                    self.help = false;
                    Some(())
                }
                _ => Some(()),
            };
        }
        // The search field owns all input while editing.
        if self.is_search_section()
            && self.region == Region::Content
            && self.mode == SearchMode::Editing
        {
            return self.editing_key(key, client);
        }
        if self.is_search_section()
            && self.region == Region::Content
            && self.mode == SearchMode::Detail
        {
            return self.detail_key(key, client);
        }

        match key.code {
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('q') => return None,
            KeyCode::Tab => {
                self.region = match self.region {
                    Region::Sidebar => Region::Content,
                    Region::Content => Region::Sidebar,
                }
            }
            KeyCode::Esc => match self.region {
                Region::Content => self.region = Region::Sidebar,
                Region::Sidebar => self.view = View::Splash,
            },
            KeyCode::Right | KeyCode::Char('l') if self.region == Region::Sidebar => {
                self.region = Region::Content
            }
            KeyCode::Left | KeyCode::Char('h') if self.region == Region::Content => {
                self.region = Region::Sidebar
            }
            _ => {}
        }
        match self.region {
            Region::Sidebar => self.sidebar_key(key),
            Region::Content => self.content_key(key, client),
        }
        Some(())
    }

    fn splash_key(&mut self, key: KeyEvent, client: &Client) -> Option<()> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => None,
            KeyCode::Enter => {
                self.enter_browser(client);
                Some(())
            }
            KeyCode::Backspace => {
                self.edit.pop();
                Some(())
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.edit.push(c);
                Some(())
            }
            _ => Some(()),
        }
    }

    fn editing_key(&mut self, key: KeyEvent, client: &Client) -> Option<()> {
        match key.code {
            KeyCode::Esc => {
                self.mode = SearchMode::List;
            }
            KeyCode::Enter => self.submit_search(client),
            KeyCode::Backspace => {
                self.edit.pop();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.edit.push(c);
            }
            _ => {}
        }
        Some(())
    }

    fn detail_key(&mut self, key: KeyEvent, client: &Client) -> Option<()> {
        match key.code {
            KeyCode::Esc => self.mode = SearchMode::List,
            KeyCode::Char('d') => {
                if let Some(r) = &self.detail {
                    self.add_result(r, client);
                }
            }
            _ => {}
        }
        Some(())
    }

    fn sidebar_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.set_section(self.section.prev()),
            KeyCode::Down | KeyCode::Char('j') => self.set_section(self.section.next()),
            KeyCode::Enter => self.region = Region::Content,
            _ => {}
        }
    }

    fn content_key(&mut self, key: KeyEvent, client: &Client) {
        if self.is_search_section() {
            // List mode only: detail/editing already handled above.
            if self.mode != SearchMode::List {
                return;
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
                KeyCode::PageUp => self.move_cursor(-10),
                KeyCode::PageDown => self.move_cursor(10),
                KeyCode::Enter => {
                    self.detail = self.visible_results().get(self.cursor).cloned();
                    self.mode = SearchMode::Detail;
                }
                KeyCode::Char('d') => {
                    if let Some(r) = self.visible_results().get(self.cursor) {
                        self.add_result(r, client);
                    }
                }
                KeyCode::Char('s') => self.sort = self.sort.next(),
                KeyCode::Char('/') => {
                    self.edit = self.query.clone();
                    self.mode = SearchMode::Editing;
                }
                _ => {}
            }
        } else {
            let visible = self.visible_torrents();
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_dl_cursor(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_dl_cursor(1),
                KeyCode::Char('p') => {
                    if let Some(v) = visible.get(self.dl_cursor) {
                        match v.status {
                            Status::Paused | Status::Queued => client.send(Action::Resume(v.id)),
                            _ => client.send(Action::Pause(v.id)),
                        }
                    }
                }
                KeyCode::Char('x') | KeyCode::Char('D') => {
                    if let Some(v) = visible.get(self.dl_cursor) {
                        let delete_files = key.code == KeyCode::Char('D');
                        client.send(Action::Remove {
                            id: v.id,
                            delete_files,
                        });
                    }
                }
                KeyCode::Char('r') => client.send(Action::Refresh),
                KeyCode::Char('P') => {
                    if let Some(v) = visible.get(self.dl_cursor) {
                        client.send(Action::Play { id: v.id });
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn result(
        name: &str,
        source: &str,
        size: u64,
        seeders: u32,
        added: Option<i64>,
    ) -> TorrentResult {
        TorrentResult {
            info_hash: format!("hash-{name}"),
            name: name.to_string(),
            size_bytes: size,
            seeders,
            leechers: 0,
            num_files: None,
            source: source.to_string(),
            magnet: format!("magnet:{name}"),
            added,
        }
    }

    fn view(id: usize, status: Status) -> TorrentView {
        TorrentView {
            id,
            info_hash: format!("ih-{id}"),
            name: format!("t{id}"),
            status,
            progress: 0.5,
            total_bytes: 1000,
            downloaded_bytes: 500,
            upload_mbps: None,
            download_mbps: Some(1.0),
            peers: 4,
            error: None,
            added_at: 0,
        }
    }

    fn client() -> (Client, mpsc::UnboundedReceiver<Action>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Client::for_test(tx), rx)
    }

    #[test]
    fn play_key_sends_play_action() {
        let (client, mut rx) = client();
        let mut app = App::new("http://test".into());
        app.view = View::Browser;
        app.torrents = vec![view(1, Status::Completed)];
        app.section = Section::Seeding;
        app.dl_cursor = 0;
        app.handle_key(key(KeyCode::Char('P')), &client).unwrap();
        assert!(matches!(rx.try_recv().unwrap(), Action::Play { id: 1 }));
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::empty())
    }

    #[test]
    fn sort_cycle_returns_to_none() {
        let mut s = Sort::None;
        for _ in 0..Sort::CYCLE.len() {
            s = s.next();
        }
        assert_eq!(s, Sort::None);
    }

    #[test]
    fn sort_by_size_desc_with_seeder_tiebreak() {
        let mut list = vec![
            result("a", "yts", 100, 2, None),
            result("b", "yts", 300, 5, None),
            result("c", "yts", 300, 9, None),
        ];
        sort_results(&mut list, Sort::SizeDesc);
        let names: Vec<&str> = list.iter().map(|r| r.name.as_str()).collect();
        // Equal sizes → more seeders first.
        assert_eq!(names, vec!["c", "b", "a"]);
    }

    #[test]
    fn sort_by_seeders_asc() {
        let mut list = vec![
            result("a", "yts", 100, 9, None),
            result("b", "yts", 100, 2, None),
        ];
        sort_results(&mut list, Sort::SeedersAsc);
        assert_eq!(list[0].seeders, 2);
        assert_eq!(list[1].seeders, 9);
    }

    #[test]
    fn sort_none_preserves_order() {
        let list = vec![
            result("a", "yts", 1, 1, None),
            result("b", "yts", 2, 2, None),
        ];
        let mut copy = list.clone();
        sort_results(&mut copy, Sort::None);
        let names: Vec<&str> = copy.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn group_filter_matches_registry() {
        assert!(in_group("yts", SourceGroup::Movies));
        assert!(in_group("nyaa", SourceGroup::Anime));
        assert!(in_group("eztv", SourceGroup::Tv));
        assert!(!in_group("yts", SourceGroup::Games));
        assert!(!in_group("unknown-source", SourceGroup::Movies));
    }

    #[test]
    fn section_navigation_wraps() {
        assert_eq!(Section::All.prev(), Section::Seeding);
        assert_eq!(Section::Seeding.next(), Section::All);
        assert_eq!(Section::Games.next(), Section::Movies);
    }

    #[test]
    fn counts_split_by_status() {
        let mut app = App::new("http://x".into());
        app.torrents = vec![
            view(1, Status::Downloading),
            view(2, Status::Paused),
            view(3, Status::Completed),
            view(4, Status::Failed),
        ];
        assert_eq!(app.active_count(), 3);
        assert_eq!(app.seeding_count(), 1);
        assert_eq!(app.active_torrents().len(), 3);
        assert_eq!(app.seeding_torrents().len(), 1);
    }

    #[test]
    fn splash_enter_submits_search_and_enters_browser() {
        let (client, mut rx) = client();
        let mut app = App::new("http://x".into());
        app.edit = "oppenheimer".into();
        app.handle_key(key(KeyCode::Enter), &client);
        assert_eq!(app.view, View::Browser);
        assert_eq!(app.section, Section::All);
        assert_eq!(app.region, Region::Content);
        assert!(app.searching);
        assert_eq!(app.query, "oppenheimer");
        match rx.try_recv() {
            Ok(Action::Search(q)) => assert_eq!(q, "oppenheimer"),
            other => panic!("expected Search action, got {other:?}"),
        }
    }

    #[test]
    fn editing_mode_owns_typing_and_q() {
        let (client, _rx) = client();
        let mut app = App::new("http://x".into());
        app.view = View::Browser;
        app.region = Region::Content;
        app.mode = SearchMode::Editing;
        // 'q' while editing must type, not quit.
        assert!(app.handle_key(key(KeyCode::Char('q')), &client).is_some());
        assert_eq!(app.edit, "q");
        assert!(app.handle_key(key(KeyCode::Esc), &client).is_some());
        assert_eq!(app.mode, SearchMode::List);
    }

    #[test]
    fn download_section_sends_pause_resume() {
        let (client, mut rx) = client();
        let mut app = App::new("http://x".into());
        app.view = View::Browser;
        app.section = Section::Downloads;
        app.region = Region::Content;
        app.torrents = vec![view(7, Status::Downloading)];
        app.dl_cursor = 0;
        app.handle_key(key(KeyCode::Char('p')), &client);
        match rx.try_recv() {
            Ok(Action::Pause(7)) => {}
            other => panic!("expected Pause(7), got {other:?}"),
        }
        app.torrents[0].status = Status::Paused;
        app.handle_key(key(KeyCode::Char('p')), &client);
        match rx.try_recv() {
            Ok(Action::Resume(7)) => {}
            other => panic!("expected Resume(7), got {other:?}"),
        }
    }

    #[test]
    fn remove_sends_delete_flag_only_for_shift_d() {
        let (client, mut rx) = client();
        let mut app = App::new("http://x".into());
        app.view = View::Browser;
        app.section = Section::Downloads;
        app.region = Region::Content;
        app.torrents = vec![view(7, Status::Downloading)];
        app.dl_cursor = 0;
        app.handle_key(key(KeyCode::Char('x')), &client);
        match rx.try_recv() {
            Ok(Action::Remove { id, delete_files }) => {
                assert_eq!(id, 7);
                assert!(!delete_files);
            }
            other => panic!("expected Remove, got {other:?}"),
        }
        app.handle_key(key(KeyCode::Char('D')), &client);
        match rx.try_recv() {
            Ok(Action::Remove { delete_files, .. }) => assert!(delete_files),
            other => panic!("expected Remove with delete, got {other:?}"),
        }
    }

    #[test]
    fn help_toggles_and_esc_closes() {
        let (client, _rx) = client();
        let mut app = App::new("http://x".into());
        app.view = View::Browser;
        assert!(app.handle_key(key(KeyCode::Char('?')), &client).is_some());
        assert!(app.help);
        assert!(app.handle_key(key(KeyCode::Esc), &client).is_some());
        assert!(!app.help);
    }

    #[test]
    fn section_switch_resets_detail() {
        let (client, _rx) = client();
        let mut app = App::new("http://x".into());
        app.view = View::Browser;
        app.region = Region::Sidebar;
        app.mode = SearchMode::Detail;
        app.detail = Some(result("x", "yts", 1, 1, None));
        app.handle_key(key(KeyCode::Down), &client);
        assert_eq!(app.section, Section::Games);
        assert_eq!(app.mode, SearchMode::List);
        assert!(app.detail.is_none());
    }
}
