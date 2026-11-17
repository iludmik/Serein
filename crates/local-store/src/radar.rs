use super::{LocalStore, Result, StoreError};
use model::{Id, RadarLog, WatchData};
use rusqlite::{OptionalExtension, params};

impl LocalStore {
	pub fn radar_watch(&self, account: Id) -> Result<WatchData> {
		let value: Option<Option<String>> = self.0.query_row(
			"SELECT CASE WHEN typeof(value)='text' AND length(CAST(value AS BLOB))<=32768 THEN value ELSE NULL END FROM radar_watch WHERE account=?1",
			[account.to_string()], |row| row.get(0)).optional()?;
		let value = match value {
			None => WatchData::default(),
			Some(None) => return Err(StoreError::Incompatible),
			Some(Some(value)) => {
				serde_json::from_str::<WatchData>(&value).map_err(|_| StoreError::Incompatible)?
			}
		};
		if !value.is_valid() {
			return Err(StoreError::Incompatible);
		}
		Ok(value)
	}

	pub fn save_radar_watch(&self, account: Id, value: &WatchData) -> Result<()> {
		if !value.is_valid() {
			return Err(StoreError::Capacity);
		}
		let value = serde_json::to_string(value).map_err(|_| StoreError::Incompatible)?;
		if value.len() > WatchData::MAX_JSON_BYTES {
			return Err(StoreError::Capacity);
		}
		self.0.execute(
			"INSERT INTO radar_watch(account,value) VALUES(?1,?2) ON CONFLICT(account) DO UPDATE SET value=excluded.value",
			params![account.to_string(), value],
		)?;
		Ok(())
	}

	pub fn radar_log(&self, account: Id) -> Result<RadarLog> {
		let value: Option<Option<String>> = self.0.query_row(
			"SELECT CASE WHEN typeof(value)='text' AND length(CAST(value AS BLOB))<=1048576 THEN value ELSE NULL END FROM radar_log WHERE account=?1",
			[account.to_string()], |row| row.get(0)).optional()?;
		let value = match value {
			None => RadarLog::default(),
			Some(None) => return Err(StoreError::Incompatible),
			Some(Some(value)) => {
				serde_json::from_str::<RadarLog>(&value).map_err(|_| StoreError::Incompatible)?
			}
		};
		if !value.is_valid() {
			return Err(StoreError::Incompatible);
		}
		Ok(value)
	}

	pub fn save_radar_log(&self, account: Id, value: &RadarLog) -> Result<()> {
		if !value.is_valid() {
			return Err(StoreError::Capacity);
		}
		let value = serde_json::to_string(value).map_err(|_| StoreError::Incompatible)?;
		if value.len() > RadarLog::MAX_JSON_BYTES {
			return Err(StoreError::Capacity);
		}
		self.0.execute(
			"INSERT INTO radar_log(account,value) VALUES(?1,?2) ON CONFLICT(account) DO UPDATE SET value=excluded.value",
			params![account.to_string(), value],
		)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use model::{RadarEventKind, WatchData};

	#[test]
	fn radar_round_trips_are_bounded_and_cleared_only_for_the_account() {
		let mut store =
			LocalStore::initialize(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
		let mut watch = WatchData::default();
		assert_eq!(
			watch.watch(Id(9), Some("Radar user"), 1),
			model::PreferenceEdit::Changed
		);
		assert_eq!(
			watch.set_name(Id(9), "nickname"),
			model::PreferenceEdit::Changed
		);
		let mut log = RadarLog::default();
		assert!(
			log.push(
				Id(9),
				RadarEventKind::Message,
				5,
				None,
				None,
				Some("general"),
				Some("hello"),
				None,
				None,
				None
			)
			.is_some()
		);
		store.save_radar_watch(Id(1), &watch).unwrap();
		store.save_radar_log(Id(1), &log).unwrap();
		assert_eq!(store.radar_watch(Id(1)).unwrap(), watch);
		assert_eq!(store.radar_log(Id(1)).unwrap(), log);
		assert_eq!(store.radar_watch(Id(2)).unwrap(), WatchData::default());
		assert_eq!(store.radar_log(Id(2)).unwrap(), RadarLog::default());
		store.forget_account(Id(1)).unwrap();
		assert_eq!(store.radar_watch(Id(1)).unwrap(), WatchData::default());
		assert_eq!(store.radar_log(Id(1)).unwrap(), RadarLog::default());
	}

	#[test]
	fn radar_rejects_oversized_and_corrupt_records() {
		let store =
			LocalStore::initialize(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
		let mut watch = WatchData::default();
		for id in 0..WatchData::MAX_ENTRIES as u64 {
			watch.watch(Id(id + 1), None, 1);
		}
		assert!(watch.is_valid());
		store.save_radar_watch(Id(1), &watch).unwrap();
		// A duplicate user makes the record invalid; the store refuses to persist it.
		watch.entries.pop();
		watch.entries.push(model::WatchEntry {
			user: watch.entries[0].user,
			..watch.entries[0].clone()
		});
		assert_eq!(
			store.save_radar_watch(Id(1), &watch),
			Err(StoreError::Capacity)
		);
		watch.entries.pop();
		store
			.0
			.execute(
				"UPDATE radar_watch SET value='not json' WHERE account='1'",
				[],
			)
			.unwrap();
		assert_eq!(store.radar_watch(Id(1)), Err(StoreError::Incompatible));
		// Bound reads even if an externally modified database bypassed its SQL constraints.
		store.save_radar_log(Id(1), &RadarLog::default()).unwrap();
		store
			.0
			.execute_batch("PRAGMA ignore_check_constraints=ON;")
			.unwrap();
		store
			.0
			.execute(
				"UPDATE radar_log SET value=?1 WHERE account='1'",
				["x".repeat(RadarLog::MAX_JSON_BYTES + 1)],
			)
			.unwrap();
		assert_eq!(store.radar_log(Id(1)), Err(StoreError::Incompatible));
	}
}
