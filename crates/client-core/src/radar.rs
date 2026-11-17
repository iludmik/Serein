//! Watched-user activity recording. A session-side diff engine over presence, voice,
//! typing and timeline updates for accounts on the local watchlist; everything lands in
//! the bounded [`model::RadarLog`] and is persisted by the desktop storage worker.

use crate::Id;
use model::{ClientPlatforms, RadarEventKind, RadarLog, RichActivity, WatchData};
use std::collections::HashMap;
use std::collections::VecDeque;

/// Typing indicators are logged at most once per window per user.
pub const TYPING_WINDOW_MS: i64 = 5_000;
/// A quick offline bounce (reconnect) inside this window is not a real status change.
pub const BLIP_WINDOW_MS: i64 = 30_000;

/// Wall-clock milliseconds for event timestamps; capture-time only, never compared to
/// `Instant`-based session clocks.
pub fn unix_ms() -> i64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |duration| duration.as_millis() as i64)
}

const MAX_NOTICES: usize = 8;
const MAX_NOTICE_BYTES: usize = 256;
/// Pending voice sessions per watched user; bounded by the watchlist.
const MAX_VOICE_SEEN: usize = 100;

/// Normalized status for diffing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKey {
	Online,
	Idle,
	Dnd,
	Offline,
}

impl StatusKey {
	pub fn from_status(status: Option<&str>) -> Self {
		match status {
			Some("idle") => Self::Idle,
			Some("dnd") => Self::Dnd,
			Some("online") => Self::Online,
			_ => Self::Offline,
		}
	}

	pub const fn key(self) -> &'static str {
		match self {
			Self::Online => "online",
			Self::Idle => "idle",
			Self::Dnd => "dnd",
			Self::Offline => "offline",
		}
	}
}

/// A pending desktop notification for a tracked event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
	pub user: Id,
	pub title: String,
	pub body: String,
}

#[derive(Clone, Debug)]
struct StatusSeen {
	key: StatusKey,
	custom: Option<String>,
	offline_at: Option<i64>,
}

#[derive(Clone, Debug)]
struct VoiceSeen {
	guild: Id,
	channel: Id,
	place: Option<String>,
	joined_at: i64,
}

/// Session-side radar state. Persistence-facing fields are public; the diffing maps are
/// bounded by the watchlist and never persisted.
#[derive(Default)]
pub struct Radar {
	pub watch: WatchData,
	pub log: RadarLog,
	watch_loaded: bool,
	log_loaded: bool,
	pub watch_dirty: bool,
	pub log_dirty: bool,
	status_seen: HashMap<Id, StatusSeen>,
	activity_seen: HashMap<Id, Option<String>>,
	voice_seen: HashMap<Id, VoiceSeen>,
	typing_seen: HashMap<Id, i64>,
	pub notices: VecDeque<Notice>,
}

impl Radar {
	pub fn watch_loaded(&self) -> bool {
		self.watch_loaded
	}

	pub fn log_loaded(&self) -> bool {
		self.log_loaded
	}

	/// New sessions start with empty diff state; restore is idempotent per account. Local
	/// edits made before the delayed load arrives always win over the stored copy.
	pub fn restore_watch(&mut self, watch: WatchData) {
		if !self.watch_dirty {
			self.watch = watch;
		}
		self.watch_dirty = false;
		self.watch_loaded = true;
		self.status_seen.clear();
		self.activity_seen.clear();
		self.voice_seen.clear();
		self.typing_seen.clear();
	}

	pub fn restore_log(&mut self, log: RadarLog) {
		if !self.log_dirty {
			self.log = log;
		}
		self.log_dirty = false;
		self.log_loaded = true;
	}

	/// Everything account-scoped dies with the session.
	pub fn logout_reset(&mut self) {
		*self = Self::default();
	}

	/// Display name for notices and log rows: nickname, then captured profile name.
	pub fn display_of(&self, user: Id) -> String {
		self.watch
			.entries
			.iter()
			.find(|entry| entry.user == user)
			.and_then(|entry| entry.name.clone().or_else(|| entry.display.clone()))
			.unwrap_or_else(|| format!("User {}", user.0))
	}

	pub fn watch(&mut self, user: Id, display: Option<&str>, now_ms: i64) -> model::PreferenceEdit {
		let edit = self.watch.watch(user, display, now_ms);
		if edit == model::PreferenceEdit::Changed {
			self.watch_dirty = true;
		}
		edit
	}

	/// Removing a user also clears their log, so no event outlives its entry.
	pub fn unwatch(&mut self, user: Id) -> bool {
		if self.watch.unwatch(user) {
			self.watch_dirty = true;
			if self.log.forget_user(user) > 0 {
				self.log_dirty = true;
			}
			self.status_seen.remove(&user);
			self.activity_seen.remove(&user);
			self.voice_seen.remove(&user);
			self.typing_seen.remove(&user);
			return true;
		}
		false
	}

	pub fn set_name(&mut self, user: Id, name: &str) -> model::PreferenceEdit {
		let edit = self.watch.set_name(user, name);
		if edit == model::PreferenceEdit::Changed {
			self.watch_dirty = true;
		}
		edit
	}

	pub fn set_pinned(&mut self, user: Id, pinned: bool) -> model::PreferenceEdit {
		let edit = self.watch.set_pinned(user, pinned);
		if edit == model::PreferenceEdit::Changed {
			self.watch_dirty = true;
		}
		edit
	}

	pub fn clear_log(&mut self) {
		if self.log.events.is_empty() {
			return;
		}
		self.log.clear();
		self.log_dirty = true;
	}

	/// Call after mutating `watch.settings` directly; marks the watchlist for persistence.
	pub fn settings_changed(&mut self) {
		self.watch_dirty = true;
	}

	fn enabled(&self, kind: RadarEventKind) -> bool {
		let settings = self.watch.settings;
		match kind {
			RadarEventKind::Message => settings.messages,
			RadarEventKind::Edit => settings.edits,
			RadarEventKind::Delete => settings.deletes,
			RadarEventKind::Typing => settings.typing,
			RadarEventKind::Status => settings.status,
			RadarEventKind::Activity => settings.activity,
			RadarEventKind::VoiceJoin | RadarEventKind::VoiceLeave => settings.voice,
			// Discord never reports other accounts' server joins or leaves.
			RadarEventKind::GuildJoin | RadarEventKind::GuildLeave => false,
		}
	}

	#[allow(clippy::too_many_arguments)]
	fn record(
		&mut self,
		user: Id,
		kind: RadarEventKind,
		now_ms: i64,
		guild: Option<Id>,
		channel: Option<Id>,
		place: Option<&str>,
		text: Option<&str>,
		previous: Option<&str>,
		platform: Option<&str>,
		message: Option<Id>,
	) {
		if !self.watch.is_watched(user) || !self.enabled(kind) {
			return;
		}
		if self
			.log
			.push(
				user, kind, now_ms, guild, channel, place, text, previous, platform, message,
			)
			.is_some()
		{
			self.log_dirty = true;
		}
		if self.watch.settings.notify {
			let title = self.display_of(user);
			let body = notice_body(kind, place, text.or(previous));
			if !body.is_empty() {
				if self.notices.len() >= MAX_NOTICES {
					self.notices.pop_front();
				}
				self.notices.push_back(Notice {
					user,
					title: truncate(&title, MAX_NOTICE_BYTES),
					body: truncate(&body, MAX_NOTICE_BYTES),
				});
			}
		}
	}

	/// Records a typed message. `place` is the resolved channel name, if known.
	#[allow(clippy::too_many_arguments)]
	pub fn observe_message(
		&mut self,
		user: Id,
		channel: Id,
		guild: Option<Id>,
		place: Option<&str>,
		content: &str,
		message: Option<Id>,
		now_ms: i64,
	) {
		if content.is_empty() {
			return;
		}
		self.record(
			user,
			RadarEventKind::Message,
			now_ms,
			guild,
			Some(channel),
			place,
			Some(content),
			None,
			None,
			message,
		);
	}

	#[allow(clippy::too_many_arguments)]
	pub fn observe_edit(
		&mut self,
		user: Id,
		channel: Id,
		guild: Option<Id>,
		place: Option<&str>,
		previous: &str,
		content: &str,
		message: Option<Id>,
		now_ms: i64,
	) {
		if content.is_empty() || previous == content {
			return;
		}
		self.record(
			user,
			RadarEventKind::Edit,
			now_ms,
			guild,
			Some(channel),
			place,
			Some(content),
			Some(previous),
			None,
			message,
		);
	}

	/// `content` is the captured pre-delete text, when the row was loaded.
	#[allow(clippy::too_many_arguments)]
	pub fn observe_delete(
		&mut self,
		user: Id,
		channel: Id,
		guild: Option<Id>,
		place: Option<&str>,
		content: Option<&str>,
		message: Option<Id>,
		now_ms: i64,
	) {
		if content.is_none() {
			return;
		}
		self.record(
			user,
			RadarEventKind::Delete,
			now_ms,
			guild,
			Some(channel),
			place,
			content,
			None,
			None,
			message,
		);
	}

	/// Typing signals are already rate-limited upstream; the window only stops repeat logs.
	pub fn observe_typing(&mut self, user: Id, now_ms: i64) {
		if !self.watch.is_watched(user) || !self.enabled(RadarEventKind::Typing) {
			return;
		}
		match self.typing_seen.get(&user) {
			Some(&last) if now_ms - last < TYPING_WINDOW_MS => return,
			_ => {}
		}
		self.typing_seen.insert(user, now_ms);
		self.record(
			user,
			RadarEventKind::Typing,
			now_ms,
			None,
			None,
			None,
			None,
			None,
			None,
			None,
		);
	}

	/// Diffs one resolved presence value. The first sighting of a watched user only
	/// establishes the baseline; nothing is logged.
	pub fn observe_presence(
		&mut self,
		user: Id,
		status: Option<&str>,
		custom_status: Option<&str>,
		clients: ClientPlatforms,
		now_ms: i64,
	) {
		if !self.watch.is_watched(user) {
			return;
		}
		let key = StatusKey::from_status(status);
		let platform = platform_label(clients);
		let Some(seen) = self.status_seen.get(&user) else {
			self.status_seen.insert(
				user,
				StatusSeen {
					key,
					custom: custom_status.map(str::to_owned),
					offline_at: None,
				},
			);
			return;
		};
		let previous = seen.clone();
		if previous.key == key {
			if previous.custom == custom_status.map(str::to_owned) {
				return;
			}
			// Status unchanged; the custom status text is the whole story.
			if let Some(seen) = self.status_seen.get_mut(&user) {
				seen.custom = custom_status.map(str::to_owned);
			}
			self.record(
				user,
				RadarEventKind::Status,
				now_ms,
				None,
				None,
				None,
				custom_status,
				previous.custom.as_deref(),
				None,
				None,
			);
			return;
		}
		// A quick offline bounce is a reconnect, not a change.
		if previous.key == StatusKey::Offline
			&& key != StatusKey::Offline
			&& previous
				.offline_at
				.is_some_and(|at| now_ms - at < BLIP_WINDOW_MS)
		{
			if let Some(seen) = self.status_seen.get_mut(&user) {
				seen.key = key;
				seen.custom = custom_status.map(str::to_owned);
				seen.offline_at = None;
			}
			return;
		}
		if let Some(seen) = self.status_seen.get_mut(&user) {
			seen.key = key;
			seen.custom = custom_status.map(str::to_owned);
			seen.offline_at = (key == StatusKey::Offline).then_some(now_ms);
		}
		self.record(
			user,
			RadarEventKind::Status,
			now_ms,
			None,
			None,
			None,
			Some(key.key()),
			Some(previous.key.key()),
			platform.as_deref(),
			None,
		);
	}

	/// Logs the first activity (game, Spotify, stream) by name when it changes. An empty
	/// list ends the previous activity.
	pub fn observe_activity(&mut self, user: Id, activities: &[RichActivity], now_ms: i64) {
		if !self.watch.is_watched(user) {
			return;
		}
		let current = activities.first().map(|activity| activity.name.as_str());
		let Some(previous) = self.activity_seen.get(&user).cloned() else {
			self.activity_seen.insert(user, current.map(str::to_owned));
			return;
		};
		if previous.as_deref() == current {
			return;
		}
		self.activity_seen.insert(user, current.map(str::to_owned));
		self.record(
			user,
			RadarEventKind::Activity,
			now_ms,
			None,
			None,
			None,
			current,
			previous.as_deref(),
			None,
			None,
		);
	}

	/// Voice roster updates: `channel` is `None` when the user left voice. Channel moves
	/// record a leave for the old channel followed by a join.
	pub fn observe_voice(
		&mut self,
		user: Id,
		guild: Option<Id>,
		channel: Option<Id>,
		place: Option<&str>,
		now_ms: i64,
	) {
		if !self.watch.is_watched(user) || !self.enabled(RadarEventKind::VoiceJoin) {
			return;
		}
		let previous = self.voice_seen.get(&user).cloned();
		match (previous, channel) {
			(None, Some(channel)) => {
				if self.voice_seen.len() >= MAX_VOICE_SEEN {
					self.voice_seen.clear();
				}
				let Some(guild) = guild else { return };
				self.voice_seen.insert(
					user,
					VoiceSeen {
						guild,
						channel,
						place: place.map(str::to_owned),
						joined_at: now_ms,
					},
				);
				self.record(
					user,
					RadarEventKind::VoiceJoin,
					now_ms,
					Some(guild),
					Some(channel),
					place,
					None,
					None,
					None,
					None,
				);
			}
			(Some(previous), None) => {
				self.voice_seen.remove(&user);
				let duration = format_duration(now_ms - previous.joined_at);
				self.record(
					user,
					RadarEventKind::VoiceLeave,
					now_ms,
					Some(previous.guild),
					Some(previous.channel),
					previous.place.as_deref(),
					Some(&duration),
					None,
					None,
					None,
				);
			}
			(Some(previous), Some(channel)) if channel != previous.channel => {
				let duration = format_duration(now_ms - previous.joined_at);
				self.record(
					user,
					RadarEventKind::VoiceLeave,
					now_ms,
					Some(previous.guild),
					Some(previous.channel),
					previous.place.as_deref(),
					Some(&duration),
					None,
					None,
					None,
				);
				if let Some(guild) = guild {
					self.voice_seen.insert(
						user,
						VoiceSeen {
							guild,
							channel,
							place: place.map(str::to_owned),
							joined_at: now_ms,
						},
					);
					self.record(
						user,
						RadarEventKind::VoiceJoin,
						now_ms,
						Some(guild),
						Some(channel),
						place,
						None,
						None,
						None,
						None,
					);
				} else {
					self.voice_seen.remove(&user);
				}
			}
			_ => {}
		}
	}

	/// Drops pending notices; called after the desktop drains them.
	pub fn take_notice(&mut self) -> Option<Notice> {
		self.notices.pop_front()
	}
}

fn platform_label(clients: ClientPlatforms) -> Option<String> {
	let mut platforms = Vec::new();
	if clients.desktop {
		platforms.push("desktop");
	}
	if clients.mobile {
		platforms.push("mobile");
	}
	if clients.web {
		platforms.push("web");
	}
	if clients.vr {
		platforms.push("vr");
	}
	match platforms.len() {
		0 => None,
		1 => Some(platforms[0].to_owned()),
		_ => Some(platforms.join("/")),
	}
}

/// Compact locale-neutral duration for voice sessions.
fn format_duration(duration_ms: i64) -> String {
	let seconds = (duration_ms.max(0) / 1000) as u64;
	if seconds < 60 {
		return format!("{seconds}s");
	}
	let minutes = seconds / 60;
	if minutes < 60 {
		return format!("{minutes}m {:02}s", seconds % 60);
	}
	format!("{}h {:02}m", minutes / 60, minutes % 60)
}

fn notice_body(kind: RadarEventKind, place: Option<&str>, text: Option<&str>) -> String {
	let mut body = match kind {
		RadarEventKind::Message => "sent a message".to_owned(),
		RadarEventKind::Edit => "edited a message".to_owned(),
		RadarEventKind::Delete => "deleted a message".to_owned(),
		RadarEventKind::Typing => "started typing".to_owned(),
		RadarEventKind::Status => "changed status".to_owned(),
		RadarEventKind::Activity => "changed activity".to_owned(),
		RadarEventKind::VoiceJoin => "joined voice".to_owned(),
		RadarEventKind::VoiceLeave => "left voice".to_owned(),
		RadarEventKind::GuildJoin => "joined a server".to_owned(),
		RadarEventKind::GuildLeave => "left a server".to_owned(),
	};
	if let Some(place) = place {
		body.push_str(" in ");
		body.push_str(place);
	}
	if let Some(text) = text {
		body.push_str(": ");
		body.push_str(text);
	}
	truncate(&body, MAX_NOTICE_BYTES)
}

fn truncate(text: &str, max: usize) -> String {
	if text.len() <= max {
		return text.to_owned();
	}
	let mut end = max;
	while !text.is_char_boundary(end) {
		end -= 1;
	}
	text[..end].to_owned()
}

#[cfg(test)]
mod tests {
	use super::*;
	use model::RadarEvent;

	fn radar() -> Radar {
		let mut radar = Radar::default();
		radar.watch.watch(Id(7), Some("Alice"), 0);
		radar
	}

	fn events(radar: &Radar) -> &[RadarEvent] {
		&radar.log.events
	}

	#[test]
	fn first_presence_sighting_is_baseline_then_changes_log_with_before_after() {
		let mut radar = radar();
		radar.observe_presence(Id(7), Some("online"), None, ClientPlatforms::default(), 100);
		assert!(events(&radar).is_empty());
		radar.observe_presence(Id(7), Some("dnd"), None, ClientPlatforms::default(), 200);
		assert_eq!(events(&radar).len(), 1);
		assert_eq!(events(&radar)[0].kind, RadarEventKind::Status);
		assert_eq!(events(&radar)[0].text.as_deref(), Some("dnd"));
		assert_eq!(events(&radar)[0].previous.as_deref(), Some("online"));
		// Same status with a new custom status logs the text change only.
		radar.observe_presence(
			Id(7),
			Some("dnd"),
			Some("working"),
			ClientPlatforms::default(),
			300,
		);
		assert_eq!(events(&radar)[0].text.as_deref(), Some("working"));
		assert_eq!(events(&radar)[0].previous, None);
		assert_eq!(
			events(&radar)[1].text.as_deref(),
			Some("dnd"),
			"log is newest first"
		);
		assert!(radar.log_dirty);
		assert!(radar.notices.is_empty());
	}

	#[test]
	fn offline_bounces_inside_the_window_are_not_changes() {
		let mut radar = radar();
		radar.observe_presence(Id(7), Some("online"), None, ClientPlatforms::default(), 0);
		radar.observe_presence(Id(7), None, None, ClientPlatforms::default(), 10_000);
		assert_eq!(events(&radar).len(), 1);
		radar.observe_presence(
			Id(7),
			Some("online"),
			None,
			ClientPlatforms::default(),
			20_000,
		);
		assert_eq!(events(&radar).len(), 1, "a 10s reconnect is not a change");
		radar.observe_presence(Id(7), None, None, ClientPlatforms::default(), 60_000);
		radar.observe_presence(
			Id(7),
			Some("online"),
			None,
			ClientPlatforms::default(),
			100_000,
		);
		assert_eq!(events(&radar).len(), 3, "a 40s gap is a real change");
	}

	#[test]
	fn typing_is_logged_once_per_window() {
		let mut radar = radar();
		radar.watch.settings.typing = true;
		radar.observe_typing(Id(7), 1_000);
		radar.observe_typing(Id(7), 3_000);
		assert_eq!(events(&radar).len(), 1);
		radar.observe_typing(Id(7), 7_000);
		assert_eq!(events(&radar).len(), 2);
	}

	#[test]
	fn voice_pairs_join_leave_with_duration_and_moves_are_split() {
		let mut radar = radar();
		radar.observe_voice(Id(7), Some(Id(1)), Some(Id(10)), Some("general"), 1_000);
		assert_eq!(events(&radar)[0].kind, RadarEventKind::VoiceJoin);
		radar.observe_voice(Id(7), Some(Id(1)), Some(Id(10)), Some("general"), 2_000);
		assert_eq!(events(&radar).len(), 1, "same channel is not an event");
		radar.observe_voice(Id(7), Some(Id(1)), Some(Id(11)), Some("lounge"), 62_000);
		assert_eq!(events(&radar)[1].kind, RadarEventKind::VoiceLeave);
		assert_eq!(events(&radar)[1].text.as_deref(), Some("1m 01s"));
		assert_eq!(events(&radar)[1].place.as_deref(), Some("general"));
		assert_eq!(
			events(&radar)[0].kind,
			RadarEventKind::VoiceJoin,
			"log is newest first"
		);
		assert_eq!(events(&radar)[0].place.as_deref(), Some("lounge"));
		radar.observe_voice(Id(7), Some(Id(1)), None, None, 70_000);
		assert_eq!(events(&radar)[0].kind, RadarEventKind::VoiceLeave);
		assert_eq!(events(&radar)[0].text.as_deref(), Some("8s"));
	}

	#[test]
	fn activity_changes_log_and_cleared_activities_end_the_previous_one() {
		let activity = |name: &str| RichActivity {
			kind: 0,
			name: name.to_owned(),
			details: None,
			state: None,
			image: None,
			small_image: None,
			started_at: None,
			ends_at: None,
		};
		let mut radar = radar();
		radar.watch.settings.activity = true;
		radar.observe_activity(Id(7), &[activity("Visual Studio Code")], 100);
		assert!(events(&radar).is_empty(), "baseline only");
		radar.observe_activity(Id(7), &[activity("Visual Studio Code")], 200);
		assert!(events(&radar).is_empty());
		radar.observe_activity(Id(7), &[activity("Spotify")], 300);
		assert_eq!(events(&radar)[0].text.as_deref(), Some("Spotify"));
		assert_eq!(
			events(&radar)[0].previous.as_deref(),
			Some("Visual Studio Code")
		);
		radar.observe_activity(Id(7), &[], 400);
		assert_eq!(events(&radar)[0].text, None, "log is newest first");
		assert_eq!(events(&radar)[0].previous.as_deref(), Some("Spotify"));
		assert_eq!(events(&radar)[1].text.as_deref(), Some("Spotify"));
	}

	#[test]
	fn disabled_categories_and_unwatched_users_record_nothing() {
		let mut radar = radar();
		radar.watch.settings.messages = false;
		radar.watch.settings.typing = false;
		radar.observe_message(Id(7), Id(1), None, None, "hello", Some(Id(5)), 100);
		radar.observe_typing(Id(7), 100);
		assert!(events(&radar).is_empty());
		assert!(!radar.log_dirty);
		radar.watch.settings.messages = true;
		radar.observe_message(Id(8), Id(1), None, None, "hello", Some(Id(5)), 100);
		assert!(events(&radar).is_empty(), "unwatched user");
		radar.observe_message(Id(7), Id(1), None, None, "hello", Some(Id(5)), 100);
		assert_eq!(events(&radar).len(), 1);
	}

	#[test]
	fn notify_queues_bounded_notices() {
		let mut radar = radar();
		radar.watch.settings.notify = true;
		for id in 0..(MAX_NOTICES + 4) as u64 {
			radar.observe_message(Id(7), Id(1), None, Some("general"), "hi", None, id as i64);
		}
		assert_eq!(radar.notices.len(), MAX_NOTICES);
		let notice = radar.take_notice().unwrap();
		assert_eq!(notice.title, "Alice");
		assert!(notice.body.contains("sent a message"));
		assert!(notice.body.contains("in general"));
	}

	#[test]
	fn unwatch_clears_the_users_log_and_diff_state() {
		let mut radar = radar();
		radar.observe_message(Id(7), Id(1), None, None, "hello", Some(Id(5)), 100);
		radar.observe_presence(Id(7), Some("online"), None, ClientPlatforms::default(), 100);
		radar.observe_presence(Id(7), Some("idle"), None, ClientPlatforms::default(), 200);
		assert!(!events(&radar).is_empty());
		assert!(radar.unwatch(Id(7)));
		assert!(events(&radar).is_empty());
		assert!(radar.log_dirty);
		assert!(!radar.watch.is_watched(Id(7)));
		// Re-watching starts a fresh baseline, not a replay.
		radar.watch.watch(Id(7), None, 300);
		radar.observe_presence(Id(7), Some("idle"), None, ClientPlatforms::default(), 400);
		assert!(events(&radar).is_empty());
	}

	#[test]
	fn duration_formatting_is_compact() {
		assert_eq!(format_duration(45_000), "45s");
		assert_eq!(format_duration(61_000), "1m 01s");
		assert_eq!(format_duration(4_446_000), "1h 14m");
	}
}
