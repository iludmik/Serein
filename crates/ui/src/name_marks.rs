//! Marks drawn after a person's name: the server owner's crown and a staff tag from the
//! member's roles' permissions.
use crate::icons::{self, Icon};
use client_core::State;
use model::Id;

/// What would be drawn after one person's name.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Marks {
	/// Whether the person owns the server the name is shown in.
	pub owner: bool,
	/// A staff tag from the person's roles in that server, as a translation key.
	pub tag: Option<&'static str>,
}

impl Marks {
	/// Collects the marks for `user` in server `guild` with the member's `roles`.
	pub(crate) fn of(
		state: &State,
		user: &model::User,
		(guild, roles): (Option<Id>, &[Id]),
	) -> Self {
		let mut marks = Self::default();
		if user.webhook || user.id.0 == 0 {
			return marks;
		}
		let Some(guild) = guild else {
			return marks;
		};
		marks.owner = state
			.permissions
			.guilds
			.get(&guild)
			.and_then(|metadata| metadata.owner)
			== Some(user.id);
		if !marks.owner {
			marks.tag = staff_tag(state, guild, roles);
		}
		marks
	}

	/// Whether there is nothing to draw.
	pub(crate) fn is_empty(&self) -> bool {
		!self.owner && self.tag.is_none()
	}

	/// Width to keep free after the name so the marks fit.
	pub(crate) fn reserve(&self) -> f32 {
		(if self.owner { 19.0 } else { 0.0 }) + if self.tag.is_some() { 44.0 } else { 0.0 }
	}

	pub(crate) fn show(&self, ui: &mut egui::Ui) {
		if self.is_empty() {
			return;
		}
		let colors = crate::design::palette(ui);
		if self.owner {
			let rect = icons::inline(ui, Icon::Crown, 14.0, colors.warning);
			ui.interact(
				rect,
				ui.scope_id().with("owner-crown"),
				egui::Sense::hover(),
			)
			.on_hover_text(crate::i18n::translate("name-marks-server-owner"));
		}
		if let Some(tag) = self.tag {
			egui::Frame::new()
				.fill(colors.raised)
				.corner_radius(3)
				.inner_margin(egui::Margin::symmetric(4, 0))
				.show(ui, |ui| {
					ui.add(
						egui::Label::new(
							egui::RichText::new(crate::i18n::translate(tag))
								.size(10.0)
								.strong()
								.color(colors.text),
						)
						.selectable(false),
					);
				});
		}
	}
}

/// "Admin" for roles that administer or manage the server, "Mod" for roles that moderate
/// messages or members, from the roles' permissions and the server's everyone role.
fn staff_tag(state: &State, guild: Id, roles: &[Id]) -> Option<&'static str> {
	use model::permissions as p;
	let all = state.permissions.guilds.get(&guild)?.roles.as_deref()?;
	let bits = all
		.iter()
		.filter(|role| role.id == guild || roles.contains(&role.id))
		.fold(0, |bits, role| bits | role.bits);
	if bits & (p::ADMINISTRATOR | p::MANAGE_GUILD) != 0 {
		Some("name-marks-tag-admin")
	} else if bits & (p::MANAGE_MESSAGES | p::KICK_MEMBERS | p::BAN_MEMBERS | p::MODERATE_MEMBERS)
		!= 0
	{
		Some("name-marks-tag-moderator")
	} else {
		None
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn role(id: u64, bits: u128) -> model::permissions::Role {
		model::permissions::Role {
			id: Id(id),
			bits,
			name: String::new(),
			color: 0,
			secondary_color: None,
			tertiary_color: None,
			position: 1,
			hoist: false,
			managed: false,
		}
	}

	/// Checks the owner crown replaces any staff tag, and that the tag follows the roles'
	/// permissions, including the server's everyone role.
	#[test]
	fn owner_crown_and_staff_tags_follow_roles() {
		let mut state = test_support::demo_state();
		let mut user = test_support::message(1, Id(22)).author;
		user.id = Id(77);
		let guild = *state.permissions.guilds.keys().next().expect("demo server");
		state.permissions.guilds.get_mut(&guild).unwrap().owner = Some(Id(76));
		let marks = |state: &State, user: &model::User, roles: &[Id]| {
			Marks::of(state, user, (Some(guild), roles))
		};
		let roles = state
			.permissions
			.guilds
			.get_mut(&guild)
			.unwrap()
			.roles
			.get_or_insert_default();
		roles.retain(|role| role.id != guild);
		roles.push(role(guild.0, 0));
		roles.push(role(501, model::permissions::MODERATE_MEMBERS));
		roles.push(role(502, model::permissions::ADMINISTRATOR));

		assert_eq!(marks(&state, &user, &[]), Marks::default());
		assert_eq!(
			marks(&state, &user, &[Id(501)]).tag,
			Some("name-marks-tag-moderator")
		);
		assert_eq!(
			marks(&state, &user, &[Id(501), Id(502)]).tag,
			Some("name-marks-tag-admin")
		);

		// The owner's crown replaces the tag.
		state.permissions.guilds.get_mut(&guild).unwrap().owner = Some(user.id);
		let owner = marks(&state, &user, &[Id(502)]);
		assert!(owner.owner);
		assert_eq!(owner.tag, None);

		// The everyone role's permissions count too.
		state.permissions.guilds.get_mut(&guild).unwrap().owner = Some(Id(76));
		state
			.permissions
			.guilds
			.get_mut(&guild)
			.unwrap()
			.roles
			.as_mut()
			.unwrap()[0]
			.bits = model::permissions::KICK_MEMBERS;
		assert_eq!(
			marks(&state, &user, &[]).tag,
			Some("name-marks-tag-moderator")
		);

		// Webhooks and unknown users carry no marks.
		user.webhook = true;
		assert!(marks(&state, &user, &[Id(502)]).is_empty());
	}
}
