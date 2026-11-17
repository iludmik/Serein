use crate::{Id, channel_preferences::PreferenceEdit};

fn bounded(text: &str, max: usize) -> String {
	if text.len() <= max {
		return text.to_owned();
	}
	let mut end = max;
	while !text.is_char_boundary(end) {
		end -= 1;
	}
	text[..end].to_owned()
}

/// One watched account. `name` is an optional local nickname and `display` the profile name
/// captured when the user was added; `user` is unique.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WatchEntry {
	pub user: Id,
	pub name: Option<String>,
	pub display: Option<String>,
	pub pinned: bool,
	/// Unix milliseconds when the user was added.
	pub added_at: i64,
}

impl Default for WatchEntry {
	fn default() -> Self {
		Self {
			user: Id(0),
			name: None,
			display: None,
			pinned: false,
			added_at: 0,
		}
	}
}

/// Which tracked categories are enabled. Off categories produce no events at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WatchSettings {
	pub messages: bool,
	pub edits: bool,
	pub deletes: bool,
	pub typing: bool,
	pub status: bool,
	pub activity: bool,
	pub voice: bool,
	pub guild: bool,
	/// Raise a desktop notification for tracked events.
	pub notify: bool,
}

impl Default for WatchSettings {
	fn default() -> Self {
		Self {
			messages: true,
			edits: true,
			deletes: true,
			typing: false,
			status: true,
			activity: false,
			voice: true,
			guild: true,
			notify: false,
		}
	}
}

/// Account-local watchlist. Never synchronized to Discord; removed on logout like other
/// account-scoped local data.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WatchData {
	pub entries: Vec<WatchEntry>,
	pub settings: WatchSettings,
}

impl WatchData {
	pub const MAX_ENTRIES: usize = 100;
	pub const MAX_NAME_BYTES: usize = 64;
	pub const MAX_DISPLAY_BYTES: usize = 128;
	pub const MAX_JSON_BYTES: usize = 32 * 1024;

	pub fn is_valid(&self) -> bool {
		self.entries.len() <= Self::MAX_ENTRIES
			&& self.entries.capacity() <= Self::MAX_ENTRIES * 2
			&& self.entries.iter().enumerate().all(|(index, entry)| {
				entry.user.0 != 0
					&& entry
						.name
						.as_ref()
						.is_none_or(|name| name.len() <= Self::MAX_NAME_BYTES)
					&& entry
						.display
						.as_ref()
						.is_none_or(|display| display.len() <= Self::MAX_DISPLAY_BYTES)
					&& !self.entries[..index]
						.iter()
						.any(|other| other.user == entry.user)
			})
	}

	pub fn is_watched(&self, user: Id) -> bool {
		user.0 != 0 && self.entries.iter().any(|entry| entry.user == user)
	}

	/// Idempotent; a watched user keeps its existing entry. `display` is the profile name
	/// the caller already resolved, stored for log readability.
	pub fn watch(&mut self, user: Id, display: Option<&str>, now_ms: i64) -> PreferenceEdit {
		if user.0 == 0 || !self.is_valid() || self.is_watched(user) {
			return PreferenceEdit::Unchanged;
		}
		if self.entries.len() >= Self::MAX_ENTRIES {
			return PreferenceEdit::CapacityReached;
		}
		let display = display
			.map(str::trim)
			.filter(|display| !display.is_empty())
			.map(|display| bounded(display, Self::MAX_DISPLAY_BYTES));
		self.entries.push(WatchEntry {
			user,
			name: None,
			display,
			pinned: false,
			added_at: now_ms,
		});
		PreferenceEdit::Changed
	}

	/// Returns whether an entry was removed.
	pub fn unwatch(&mut self, user: Id) -> bool {
		if user.0 == 0 {
			return false;
		}
		let before = self.entries.len();
		self.entries.retain(|entry| entry.user != user);
		before != self.entries.len()
	}

	/// Sets the local nickname; an empty name clears it. Returns whether anything changed.
	pub fn set_name(&mut self, user: Id, name: &str) -> PreferenceEdit {
		let trimmed = name.trim();
		let name = (!trimmed.is_empty()).then(|| bounded(trimmed, Self::MAX_NAME_BYTES));
		let Some(entry) = self.entries.iter_mut().find(|entry| entry.user == user) else {
			return PreferenceEdit::Unchanged;
		};
		if entry.name == name {
			return PreferenceEdit::Unchanged;
		}
		entry.name = name;
		PreferenceEdit::Changed
	}

	pub fn set_pinned(&mut self, user: Id, pinned: bool) -> PreferenceEdit {
		let Some(entry) = self.entries.iter_mut().find(|entry| entry.user == user) else {
			return PreferenceEdit::Unchanged;
		};
		if entry.pinned == pinned {
			return PreferenceEdit::Unchanged;
		}
		entry.pinned = pinned;
		PreferenceEdit::Changed
	}
}

/// What happened. Variants carry their details in the shared display fields of
/// [`RadarEvent`] so the log stays a flat, bounded record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadarEventKind {
	Message,
	Edit,
	Delete,
	Typing,
	Status,
	VoiceJoin,
	VoiceLeave,
	GuildJoin,
	GuildLeave,
	Activity,
}

impl RadarEventKind {
	pub const ALL: [Self; 10] = [
		Self::Message,
		Self::Edit,
		Self::Delete,
		Self::Typing,
		Self::Status,
		Self::VoiceJoin,
		Self::VoiceLeave,
		Self::GuildJoin,
		Self::GuildLeave,
		Self::Activity,
	];

	/// Stable wire key for serde and i18n lookup.
	pub const fn key(self) -> &'static str {
		match self {
			Self::Message => "message",
			Self::Edit => "edit",
			Self::Delete => "delete",
			Self::Typing => "typing",
			Self::Status => "status",
			Self::VoiceJoin => "voice_join",
			Self::VoiceLeave => "voice_leave",
			Self::GuildJoin => "guild_join",
			Self::GuildLeave => "guild_leave",
			Self::Activity => "activity",
		}
	}
}

/// One recorded occurrence, with display data resolved at capture time: watched users'
/// presence and membership may vanish later, so names and text are copied in.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RadarEvent {
	pub id: u64,
	pub user: Id,
	pub kind: RadarEventKind,
	/// Unix milliseconds.
	pub at: i64,
	pub guild: Option<Id>,
	pub channel: Option<Id>,
	/// Resolved channel or server name for display.
	pub place: Option<String>,
	/// New value: message text, new status, activity name.
	pub text: Option<String>,
	/// Previous value for before/after kinds (edits, status, activity).
	pub previous: Option<String>,
	/// Reported platform or client, when known.
	pub platform: Option<String>,
	/// The message this event refers to, for jump-to-message.
	pub message: Option<Id>,
}

impl Default for RadarEvent {
	fn default() -> Self {
		Self {
			id: 0,
			user: Id(0),
			kind: RadarEventKind::Message,
			at: 0,
			guild: None,
			channel: None,
			place: None,
			text: None,
			previous: None,
			platform: None,
			message: None,
		}
	}
}

impl RadarEvent {
	pub const MAX_PLACE_BYTES: usize = 128;
	pub const MAX_TEXT_BYTES: usize = 2048;
	pub const MAX_PLATFORM_BYTES: usize = 16;

	/// Rough serialized cost, used to bound queues and stores between exact checks.
	pub fn bytes(&self) -> usize {
		96 + self.place.as_ref().map_or(0, |value| value.len())
			+ self.text.as_ref().map_or(0, |value| value.len())
			+ self.previous.as_ref().map_or(0, |value| value.len())
			+ self.platform.as_ref().map_or(0, |value| value.len())
	}
}

/// Bounded activity log for watched users, newest first. Account-local; removed on logout.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RadarLog {
	pub events: Vec<RadarEvent>,
	/// Next monotonic event id, never reused across evictions.
	pub next_id: u64,
}

impl RadarLog {
	pub const MAX_EVENTS: usize = 2000;
	/// The store rejects larger blobs; `push` evicts oldest events to stay inside it.
	pub const MAX_JSON_BYTES: usize = 1024 * 1024;
	/// Headroom for JSON escaping and field names on top of the summed `RadarEvent::cost`.
	const COST_HEADROOM: usize = 256 * 1024;

	pub fn is_valid(&self) -> bool {
		self.next_id >= self.events.iter().map(|event| event.id).max().unwrap_or(0)
			&& self.events.len() <= Self::MAX_EVENTS
			&& self.events.capacity() <= Self::MAX_EVENTS * 2
			&& self.events.iter().all(|event| {
				event.user.0 != 0
					&& event.id != 0
					&& event
						.place
						.as_ref()
						.is_none_or(|value| value.len() <= RadarEvent::MAX_PLACE_BYTES)
					&& event
						.text
						.as_ref()
						.is_none_or(|value| value.len() <= RadarEvent::MAX_TEXT_BYTES)
					&& event
						.previous
						.as_ref()
						.is_none_or(|value| value.len() <= RadarEvent::MAX_TEXT_BYTES)
					&& event
						.platform
						.as_ref()
						.is_none_or(|value| value.len() <= RadarEvent::MAX_PLATFORM_BYTES)
			})
	}

	fn approx_bytes(&self) -> usize {
		Self::COST_HEADROOM + self.events.iter().map(RadarEvent::bytes).sum::<usize>()
	}

	/// Records one event at the front, evicting the oldest entries while the byte estimate
	/// exceeds the store budget. `id` is assigned monotonically; oversized strings are
	/// truncated on a character boundary. Returns the assigned id, or `None` when `user`
	/// is invalid.
	#[allow(clippy::too_many_arguments)]
	pub fn push(
		&mut self,
		user: Id,
		kind: RadarEventKind,
		at: i64,
		guild: Option<Id>,
		channel: Option<Id>,
		place: Option<&str>,
		text: Option<&str>,
		previous: Option<&str>,
		platform: Option<&str>,
		message: Option<Id>,
	) -> Option<u64> {
		if user.0 == 0 || !self.is_valid() {
			return None;
		}
		let id = self.next_id.checked_add(1)?;
		self.next_id = id;
		let mut event = RadarEvent {
			id,
			user,
			kind,
			at,
			guild,
			channel,
			place: place.map(|value| bounded(value, RadarEvent::MAX_PLACE_BYTES)),
			text: text.map(|value| bounded(value, RadarEvent::MAX_TEXT_BYTES)),
			previous: previous.map(|value| bounded(value, RadarEvent::MAX_TEXT_BYTES)),
			platform: platform.map(|value| bounded(value, RadarEvent::MAX_PLATFORM_BYTES)),
			message,
		};
		// A single oversized event is clamped by the string bounds; drop everything older
		// if even that cannot fit.
		if event.bytes() + Self::COST_HEADROOM > Self::MAX_JSON_BYTES {
			event.place = None;
			event.previous = None;
			let clamped = event.text.take().map_or_else(String::new, |value| {
				bounded(&value, RadarEvent::MAX_TEXT_BYTES / 8)
			});
			event.text = Some(clamped);
		}
		while self.events.len() >= Self::MAX_EVENTS {
			self.events.pop();
		}
		self.events.insert(0, event);
		while self.events.len() > 1 && self.approx_bytes() > Self::MAX_JSON_BYTES {
			self.events.pop();
		}
		Some(id)
	}

	pub fn clear(&mut self) {
		self.events.clear();
		self.events.shrink_to_fit();
	}

	/// Drops every event of one user; returns how many were removed.
	pub fn forget_user(&mut self, user: Id) -> usize {
		let before = self.events.len();
		self.events.retain(|event| event.user != user);
		if before != self.events.len() {
			self.events.shrink_to_fit();
		}
		before - self.events.len()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn watch_bounds_and_idempotence() {
		let mut data = WatchData::default();
		assert_eq!(data.watch(Id(0), None, 1), PreferenceEdit::Unchanged);
		assert_eq!(data.watch(Id(1), Some("Alice"), 1), PreferenceEdit::Changed);
		assert_eq!(
			data.watch(Id(1), Some("Alice"), 2),
			PreferenceEdit::Unchanged
		);
		for id in 2..WatchData::MAX_ENTRIES as u64 + 1 {
			assert_eq!(data.watch(Id(id), None, 3), PreferenceEdit::Changed);
		}
		assert_eq!(
			data.watch(Id(9_999), None, 4),
			PreferenceEdit::CapacityReached
		);
		assert!(data.is_valid());
		assert!(data.unwatch(Id(1)));
		assert!(!data.unwatch(Id(1)));
		assert_eq!(data.watch(Id(9_999), None, 5), PreferenceEdit::Changed);
	}

	#[test]
	fn nickname_is_bounded_and_clearable() {
		let mut data = WatchData::default();
		data.watch(Id(5), Some("Bob"), 1);
		assert_eq!(data.entries[0].display.as_deref(), Some("Bob"));
		let long = "é".repeat(WatchData::MAX_NAME_BYTES);
		assert_eq!(data.set_name(Id(5), &long), PreferenceEdit::Changed);
		let stored = data.entries[0].name.clone().unwrap();
		assert!(stored.len() <= WatchData::MAX_NAME_BYTES);
		assert!(stored.ends_with('é') || stored.is_empty());
		assert_eq!(data.set_name(Id(5), "  "), PreferenceEdit::Changed);
		assert_eq!(data.entries[0].name, None);
	}

	#[test]
	fn log_evicts_oldest_under_item_and_byte_budgets() {
		let mut log = RadarLog::default();
		let text = "x".repeat(RadarEvent::MAX_TEXT_BYTES);
		let pushes = RadarLog::MAX_EVENTS as u64 + 50;
		for id in 0..pushes {
			let pushed = log.push(
				Id(7),
				RadarEventKind::Message,
				id as i64,
				None,
				None,
				None,
				Some(&text),
				None,
				None,
				None,
			);
			assert_eq!(pushed, Some(id + 1));
		}
		assert!(log.events.len() <= RadarLog::MAX_EVENTS);
		assert_eq!(log.events[0].id, pushes);
		assert_eq!(log.next_id, pushes);
		assert!(log.approx_bytes() <= RadarLog::MAX_JSON_BYTES);
		assert!(log.is_valid());
		// Oldest events were evicted, newest retained in order.
		assert!(log.events.windows(2).all(|pair| pair[0].id > pair[1].id));
	}

	#[test]
	fn log_roundtrips_through_serde() {
		let mut log = RadarLog::default();
		log.push(
			Id(9),
			RadarEventKind::Edit,
			42,
			Some(Id(3)),
			Some(Id(4)),
			Some("general"),
			Some("after"),
			Some("before"),
			Some("desktop"),
			Some(Id(77)),
		);
		let encoded = serde_json::to_vec(&log).unwrap();
		assert!(encoded.len() <= RadarLog::MAX_JSON_BYTES);
		let decoded: RadarLog = serde_json::from_slice(&encoded).unwrap();
		assert_eq!(decoded, log);
		assert!(decoded.is_valid());
		assert_eq!(decoded.events[0].kind, RadarEventKind::Edit);
		assert_eq!(decoded.events[0].previous.as_deref(), Some("before"));
	}

	#[test]
	fn forget_user_removes_only_that_users_events() {
		let mut log = RadarLog::default();
		log.push(
			Id(1),
			RadarEventKind::Message,
			1,
			None,
			None,
			None,
			Some("a"),
			None,
			None,
			None,
		);
		log.push(
			Id(2),
			RadarEventKind::Status,
			2,
			None,
			None,
			None,
			Some("online"),
			Some("offline"),
			None,
			None,
		);
		assert_eq!(log.forget_user(Id(1)), 1);
		assert_eq!(log.events.len(), 1);
		assert_eq!(log.events[0].user, Id(2));
		assert_eq!(log.forget_user(Id(3)), 0);
	}
}
