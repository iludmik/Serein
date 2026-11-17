use crate::{MessagingUi, design, dialog, i18n};
use client_core::{Command, State, radar};
use model::{Id, RadarEvent, RadarEventKind};

/// Watchlist and activity log dialog state; `open` mirrors the header toggle.
#[derive(Default)]
pub struct RadarUi {
	pub open: bool,
	add_id: String,
	rename_for: Option<Id>,
	rename_buffer: String,
	filter: Option<RadarEventKind>,
}

impl RadarUi {
	fn reset_edit(&mut self) {
		self.rename_for = None;
		self.rename_buffer.clear();
		self.add_id.clear();
	}

	fn kind_label(kind: RadarEventKind) -> String {
		i18n::translate(&format!("radar-kind-{}", kind.key()))
	}
}

fn ago(at_ms: i64, now_ms: i64) -> String {
	let seconds = ((now_ms - at_ms).max(0) / 1000) as u64;
	match seconds {
		0..=59 => format!("{seconds}s"),
		60..=3599 => format!("{}m", seconds / 60),
		3600..=86399 => format!("{}h", seconds / 3600),
		_ => format!("{}d", seconds / 86400),
	}
}

fn place_label(state: &State, event: &RadarEvent) -> Option<String> {
	let channel = state.channel(event.channel?)?;
	Some(
		channel
			.guild
			.and_then(|guild| state.guild(guild))
			.map_or_else(
				|| channel.name.clone(),
				|guild| format!("{} · #{}", guild.name, channel.name),
			),
	)
}

impl MessagingUi {
	pub(super) fn radar(
		&mut self,
		ctx: &egui::Context,
		state: &mut State,
		commands: &mut Vec<Command>,
	) {
		if !self.radar.open {
			return;
		}
		let now_ms = radar::unix_ms();
		let mut add: Option<Id> = None;
		let mut remove: Option<Id> = None;
		let mut rename: Option<(Id, String)> = None;
		let mut pin: Option<(Id, bool)> = None;
		let mut clear = false;
		let mut jump: Option<RadarEvent> = None;
		let response = dialog::Dialog::new("radar", i18n::translate("radar-title"))
			.subtitle(i18n::translate("radar-description"))
			.width(640.0)
			.show(ctx, |body| {
				body.scroll(360.0, |ui| {
					let colors = design::palette(ui);
					ui.label(
						design::semibold(
							ui,
							format!(
								"{} ({}/{})",
								i18n::translate("radar-watchlist"),
								state.radar.watch.entries.len(),
								model::WatchData::MAX_ENTRIES
							),
							14.0,
						)
						.color(colors.text_strong),
					);
					// Add by ID; user context menus add with the resolved profile name.
					ui.horizontal(|ui| {
						ui.add(
							egui::TextEdit::singleline(&mut self.radar.add_id)
								.hint_text(i18n::translate("radar-add-hint"))
								.desired_width(180.0),
						);
						if !self.radar.add_id.trim().is_empty()
							&& ui.button(i18n::translate("radar-add")).clicked()
							&& let Ok(id) = self.radar.add_id.trim().parse::<u64>()
							&& id != 0
						{
							add = Some(Id(id));
						}
					});
					ui.separator();
					let mut rename_target = self.radar.rename_for;
					for entry in &state.radar.watch.entries {
						ui.horizontal(|ui| {
							let label = entry
								.name
								.clone()
								.or_else(|| entry.display.clone())
								.unwrap_or_else(|| format!("User {}", entry.user.0));
							if entry.pinned {
								ui.label(i18n::translate("radar-pinned"));
							}
							ui.label(design::semibold(ui, &label, 13.0));
							ui.with_layout(
								egui::Layout::right_to_left(egui::Align::Center),
								|ui| {
									if ui.small_button(i18n::translate("radar-remove")).clicked() {
										remove = Some(entry.user);
									}
									let pin_label = i18n::translate(if entry.pinned {
										"radar-unpin"
									} else {
										"radar-pin"
									});
									if ui.small_button(pin_label).clicked() {
										pin = Some((entry.user, !entry.pinned));
									}
									if ui.small_button(i18n::translate("radar-rename")).clicked() {
										rename_target = Some(entry.user);
										self.radar.rename_buffer =
											entry.name.clone().unwrap_or_default();
									}
								},
							);
						});
						if rename_target == Some(entry.user) {
							ui.horizontal(|ui| {
								ui.add(
									egui::TextEdit::singleline(&mut self.radar.rename_buffer)
										.hint_text(i18n::translate("radar-nickname"))
										.desired_width(180.0),
								);
								if ui.button(i18n::translate("radar-save")).clicked() {
									rename = Some((entry.user, self.radar.rename_buffer.clone()));
								}
							});
						}
						ui.separator();
					}
					self.radar.rename_for = rename_target;
					ui.add_space(6.0);
					ui.label(
						design::semibold(ui, i18n::translate("radar-settings"), 14.0)
							.color(colors.text_strong),
					);
					let mut settings_changed = false;
					{
						let settings = &mut state.radar.watch.settings;
						for (field, label) in [
							(&mut settings.messages, "radar-setting-messages"),
							(&mut settings.edits, "radar-setting-edits"),
							(&mut settings.deletes, "radar-setting-deletes"),
							(&mut settings.typing, "radar-setting-typing"),
							(&mut settings.status, "radar-setting-status"),
							(&mut settings.activity, "radar-setting-activity"),
							(&mut settings.voice, "radar-setting-voice"),
							(&mut settings.guild, "radar-setting-guild"),
							(&mut settings.notify, "radar-setting-notify"),
						] {
							if design::switch(ui, label, None, field).changed() {
								settings_changed = true;
							}
						}
					}
					if settings_changed {
						state.radar.settings_changed();
					}
					design::hint(ui, &i18n::translate("radar-setting-guild-detail"));
					ui.add_space(6.0);
					ui.label(
						design::semibold(
							ui,
							format!(
								"{} ({})",
								i18n::translate("radar-log"),
								state.radar.log.events.len()
							),
							14.0,
						)
						.color(colors.text_strong),
					);
					ui.horizontal_wrapped(|ui| {
						let mut select =
							|ui: &mut egui::Ui, filter: Option<RadarEventKind>, label: String| {
								let active = self.radar.filter == filter;
								if ui.selectable_label(active, label).clicked() {
									self.radar.filter = filter;
								}
							};
						select(ui, None, i18n::translate("radar-filter-all"));
						for kind in RadarEventKind::ALL {
							select(ui, Some(kind), RadarUi::kind_label(kind));
						}
					});
					let events = state
						.radar
						.log
						.events
						.iter()
						.filter(|event| self.radar.filter.is_none_or(|filter| event.kind == filter))
						.take(250)
						.cloned()
						.collect::<Vec<_>>();
					if events.is_empty() {
						ui.label(i18n::translate(if state.radar.log_loaded() {
							"radar-empty"
						} else {
							"radar-loading"
						}));
					}
					for event in &events {
						let kind = RadarUi::kind_label(event.kind);
						let place = place_label(state, event).or_else(|| event.place.clone());
						ui.horizontal_wrapped(|ui| {
							ui.weak(format!(
								"{} · {} · {}",
								ago(event.at, now_ms),
								kind,
								place.unwrap_or_default()
							));
						});
						if let Some(text) = &event.text {
							ui.label(text);
						}
						if let Some(previous) = &event.previous {
							ui.weak(format!(
								"{} {}",
								i18n::translate("radar-previous"),
								previous
							));
						}
						if event.message.is_some()
							&& ui
								.small_button(i18n::translate("radar-open-message"))
								.clicked()
						{
							jump = Some(event.clone());
						}
						ui.separator();
					}
					if state.radar.log.events.len() > 250 {
						ui.weak(i18n::translate("radar-truncated"));
					}
				});
				body.footer(|ui| {
					if ui.button(i18n::translate("radar-clear")).clicked() {
						clear = true;
					}
				});
			});
		if let Some(id) = add {
			if state.radar.watch(id, None, now_ms) == model::PreferenceEdit::CapacityReached {
				self.toasts
					.push(design::Level::Warning, i18n::translate("radar-cap"));
			}
			self.radar.reset_edit();
		}
		if let Some((id, name)) = rename {
			state.radar.set_name(id, &name);
			self.radar.reset_edit();
		}
		if let Some((id, pinned)) = pin {
			state.radar.set_pinned(id, pinned);
		}
		if let Some(id) = remove {
			state.radar.unwatch(id);
		}
		if clear {
			state.radar.clear_log();
		}
		if let Some(event) = jump
			&& let (Some(message), Some(channel)) = (event.message, event.channel)
		{
			match state.open_chat_link(event.guild, channel, Some(message)) {
				Ok(command) => {
					commands.extend(command);
					self.guild = event.guild;
					self.radar.open = false;
				}
				Err(error) => state.status = error,
			}
		}
		if response.close {
			self.radar.open = false;
			self.radar.reset_edit();
		}
	}
}
