//! Inline player: one bounded texture with Discord-style overlay controls. The desktop owns
//! media decoding and playback; this module only draws frames and emits commands.
use model::{Attachment, Id, Message};
use std::collections::{HashMap, HashSet, VecDeque};

/// Poster pixel budget: 8 MiB RGBA, about three dozen 320px posters. Oldest goes
/// first; evicted attachments may request again, cached ones never re-download.
const POSTER_BUDGET: usize = 8 * 1024 * 1024;
/// Poster count ceiling, so tiny clips cannot pile up textures under the byte budget.
const POSTER_MAX: usize = 128;
/// Remembered undecodable posters; past this the failures are forgotten and may retry.
const POSTER_SEEN_MAX: usize = 1024;

/// Reinterprets decoded RGBA as texels without a per-pixel pass. Every backend emits
/// opaque frames, where premultiplied and straight alpha are the same bytes; a per-pixel
/// `from_rgba_unmultiplied` cost a full core at 1080p30 in debug builds.
fn video_pixels(rgba: &[u8]) -> &[egui::Color32] {
	bytemuck::cast_slice(rgba)
}

const CORNER: u8 = 8;
const MAX_WIDTH: f32 = crate::avatars::media::MEDIA_MAX_WIDTH;
const MAX_HEIGHT: f32 = crate::avatars::media::MEDIA_MAX_HEIGHT;
/// X's official embed is the whole post, not just its media: header, text, the clip and
/// the footer. A stage sized to the clip alone hides the player behind that chrome and
/// makes the webview scroll internally, so X posts also reserve the chrome around the
/// clip (measured at the embed's 550 px width) plus the 30 px inline header the player
/// draws above the webview. The bound fits a typical short post without leaving a large
/// empty strip; a longer post may scroll for its footer.
const X_CHROME_HEIGHT: f32 = 300.0;
const BAR_HEIGHT: f32 = 60.0;
/// Decoded pictures share the platform's inline bound: 4K worth of pixels in a 3840 px square.
const MAX_FRAME_EDGE: usize = 3840;
const MAX_FRAME_PIXELS: usize = 3840 * 2160;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VideoState {
	#[default]
	Idle,
	Loading,
	Playing,
	Paused,
	Ended,
	Failed(&'static str),
}
/// A provider player page (YouTube/Vimeo) the desktop hosts in a temporary webview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebVideo {
	/// Validated provider embed URL; the only page the webview opens.
	pub player: String,
	/// The shared link, offered as "Open in browser".
	pub page: Option<String>,
	pub title: String,
	pub provider: String,
	pub width: u32,
	pub height: u32,
}
/// How the open provider player is presented.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum WebMode {
	/// Playing in the embed box inside the conversation.
	#[default]
	Inline,
	/// The floating corner player that leaves the conversation usable.
	Mini,
}

pub enum VideoCommand {
	Play(Attachment),
	Pause(bool),
	Seek(f64),
	Volume(f32),
	Stop,
}
pub struct VideoUi {
	pub active: Option<(Id, Id, Attachment)>,
	pub state: VideoState,
	pub position: f64,
	pub duration: f64,
	pub command: Option<VideoCommand>,
	pub seen: bool,
	pub volume: f32,
	/// Latest frame; usually the desktop's GPU-converted texture, else [`Self::managed`].
	texture: Option<egui::load::SizedTexture>,
	/// CPU fallback texture behind [`VideoUi::accept_frame`].
	managed: Option<egui::TextureHandle>,
	/// Staging pixels for the current frame. The renderer drops its reference after the
	/// upload, so the same allocation is refilled each frame instead of reallocating up to
	/// up to 33 MB per 4K frame (1080p at 60 fps alone churned ~500 MB/s through the allocator).
	frame: Option<std::sync::Arc<egui::ColorImage>>,
	/// Vertical transparent-to-black ramp behind the overlay controls.
	shade: Option<egui::TextureHandle>,
	/// Decoded first frames by attachment id, painted on idle stages instead of
	/// black. Byte-bounded below; Discord ships no video placeholders to reuse.
	pub(crate) posters: HashMap<Id, (egui::TextureHandle, usize)>,
	/// Oldest-first poster insertion order for budget eviction.
	poster_order: VecDeque<Id>,
	/// Live poster bytes against [`POSTER_BUDGET`].
	poster_bytes: usize,
	/// Attachments already asked (or answered) for a poster; no retry until eviction.
	pub(crate) poster_seen: HashSet<Id>,
	/// Idle attachments awaiting one decoded poster. Drained by the desktop each
	/// frame through its own queue so poster work never consumes the single
	/// playback command slot (and a later click cannot strand a request).
	pub poster_queue: Vec<Attachment>,
	/// Keyboard focus rested on an overlay control last frame, so keep the overlay visible.
	controls_focused: bool,
	/// Volume to restore when the mute button is pressed again.
	unmuted_volume: Option<f32>,
	/// Keep the viewport's previous mode so leaving playback restores the window.
	fullscreen: Option<(egui::Context, bool, egui::Id)>,
	/// Native window transition for the desktop to apply after this UI frame.
	pub(super) fullscreen_request: Option<bool>,
	/// The open provider player; the desktop places its webview over `web_bounds`.
	pub web: Option<WebVideo>,
	/// How the open player is presented; embed or floating mini player.
	pub web_mode: WebMode,
	/// Selection, source channel/message and media fingerprint; edits revoke the prior Play.
	web_source: Option<(Option<Id>, Id, Id, egui::Id)>,
	pub web_bounds: Option<egui::Rect>,
	/// Last size of the mini player's video area, so a resized one reopens the same way.
	web_mini_size: Option<egui::Vec2>,
	/// Last top-left of the mini player, so a moved one reopens where the user left it.
	web_mini_position: Option<egui::Pos2>,
	/// Painted in the player stage when no webview can be shown (offline demo).
	pub web_notice: Option<&'static str>,
	/// A link the provider player asked to open; routed through the link confirmation.
	pub web_external: Option<String>,
}
impl Default for VideoUi {
	fn default() -> Self {
		Self {
			active: None,
			state: VideoState::Idle,
			position: 0.0,
			duration: 0.0,
			command: None,
			seen: false,
			volume: 1.0,
			texture: None,
			managed: None,
			frame: None,
			shade: None,
			posters: HashMap::new(),
			poster_order: VecDeque::new(),
			poster_bytes: 0,
			poster_seen: HashSet::new(),
			poster_queue: Vec::new(),
			controls_focused: false,
			unmuted_volume: None,
			fullscreen: None,
			fullscreen_request: None,
			web: None,
			web_mode: WebMode::default(),
			web_source: None,
			web_bounds: None,
			web_mini_size: None,
			web_mini_position: None,
			web_notice: None,
			web_external: None,
		}
	}
}
impl VideoUi {
	pub fn stop(&mut self) {
		self.exit_fullscreen();
		self.active = None;
		self.texture = None;
		self.managed = None;
		self.frame = None;
		self.state = VideoState::Idle;
		self.position = 0.0;
		self.duration = 0.0;
		self.seen = false;
		self.command = Some(VideoCommand::Stop);
		self.web_mode = WebMode::default();
		self.web_bounds = None;
	}
	pub(super) fn exit_fullscreen(&mut self) {
		if let Some((ctx, previous, focus)) = self.fullscreen.take() {
			self.fullscreen_request = Some(previous);
			ctx.memory_mut(|memory| memory.request_focus(focus));
			ctx.request_repaint();
		}
	}
	pub fn take_fullscreen_request(&mut self) -> Option<bool> {
		self.fullscreen_request.take()
	}
	pub(super) fn is_fullscreen(&self) -> bool {
		self.fullscreen.is_some()
	}
	#[allow(clippy::too_many_arguments)]
	pub(super) fn show_fullscreen(
		&mut self,
		ctx: &egui::Context,
		message: &Message,
		attachment: &Attachment,
		original: Option<&str>,
		download: &mut crate::DownloadUi,
		opening: &mut Option<String>,
		demo: bool,
	) {
		// A modal sizing pass is invisible; it must not stop the active decoder.
		self.seen = true;
		let screen = ctx.content_rect();
		let id = egui::Id::unique("video-fullscreen");
		let overlay = egui::Modal::new(id)
			.area(
				egui::Modal::default_area(id)
					.anchor(egui::Align2::LEFT_TOP, egui::Vec2::ZERO)
					.fade_in(false),
			)
			.backdrop_color(egui::Color32::BLACK)
			.frame(egui::Frame::NONE)
			.show(ctx, |ui| {
				ui.set_min_size(screen.size());
				ui.set_max_size(screen.size());
				let response = self.show_player(
					ui, message, attachment, true, original, false, download, opening, demo,
				);
				if !is_embedded(attachment) {
					crate::attachments::media_context_menu(
						&response, attachment, download, opening, demo,
					);
				}
			});
		// The shared link confirmation is drawn before the timeline. Leave the video
		// overlay when opening an original so that confirmation remains visible.
		if overlay.should_close() || opening.is_some() {
			self.exit_fullscreen();
		}
	}
	/// The desktop rejects stale session/player frames before handing over decoded pixels.
	pub fn accept_frame(
		&mut self,
		ctx: &egui::Context,
		width: usize,
		height: usize,
		rgba: &[u8],
	) -> bool {
		if self.active.is_none()
			|| width == 0
			|| height == 0
			|| width > MAX_FRAME_EDGE
			|| height > MAX_FRAME_EDGE
			|| width * height > MAX_FRAME_PIXELS
			|| width.max(height) > ctx.input(|i| i.max_texture_side)
			|| rgba.len() != width * height * 4
		{
			return false;
		}
		let frame = self.frame.get_or_insert_with(|| {
			std::sync::Arc::new(egui::ColorImage::filled(
				[width, height],
				egui::Color32::BLACK,
			))
		});
		// Reuses the buffer once the previous upload released it; clones only if the
		// renderer still holds the last frame.
		let image = std::sync::Arc::make_mut(frame);
		let pixels = video_pixels(rgba);
		if image.resolution() == [width, height] {
			image.pixels_mut().copy_from_slice(pixels);
		} else {
			*image = egui::ColorImage::new([width, height], pixels.to_vec());
		}
		let image = std::sync::Arc::clone(frame);
		let texture = match &mut self.managed {
			Some(texture) => {
				texture.set(image, egui::TextureOptions::LINEAR);
				texture
			}
			None => self.managed.insert(ctx.load_texture(
				"inline-video",
				image,
				egui::TextureOptions::LINEAR,
			)),
		};
		self.texture = Some(egui::load::SizedTexture::from_handle(texture));
		true
	}
	/// Shows a frame the desktop already uploaded (and converted) on the GPU.
	pub fn show_frame(&mut self, texture: egui::load::SizedTexture) -> bool {
		if self.active.is_none() {
			return false;
		}
		self.texture = Some(texture);
		true
	}
	fn toggle(&mut self, message: &Message, attachment: &Attachment, state: VideoState) {
		self.command = Some(match state {
			VideoState::Loading => {
				self.stop();
				VideoCommand::Stop
			}
			VideoState::Playing => VideoCommand::Pause(true),
			VideoState::Paused => VideoCommand::Pause(false),
			_ => {
				self.active = Some((message.channel, message.id, attachment.clone()));
				self.texture = None;
				self.managed = None;
				self.frame = None;
				self.state = VideoState::Loading;
				self.position = 0.0;
				self.duration = 0.0;
				VideoCommand::Play(attachment.clone())
			}
		});
	}
	fn shade(&mut self, ctx: &egui::Context) -> egui::TextureId {
		self.shade
			.get_or_insert_with(|| {
				let pixels = (0..16)
					.map(|row| egui::Color32::from_black_alpha((row * 200 / 15) as u8))
					.collect();
				ctx.load_texture(
					"inline-video-shade",
					egui::ColorImage::new([1, 16], pixels),
					egui::TextureOptions::LINEAR,
				)
			})
			.id()
	}
	/// Caches one decoded poster frame; same bounds as [`VideoUi::accept_frame`]
	/// but independent of the active playback, so idle stages keep their preview.
	pub fn accept_poster(
		&mut self,
		ctx: &egui::Context,
		id: Id,
		width: usize,
		height: usize,
		rgba: &[u8],
	) -> bool {
		if width == 0
			|| height == 0
			|| width > MAX_FRAME_EDGE
			|| height > MAX_FRAME_EDGE
			|| width * height > MAX_FRAME_PIXELS
			|| width.max(height) > ctx.input(|i| i.max_texture_side)
			|| rgba.len() != width * height * 4
		{
			return false;
		}
		let bytes = rgba.len();
		// Reinserting refreshes recency; eviction below drops textures (GPU freed).
		self.evict_poster(&id);
		while self.poster_bytes + bytes > POSTER_BUDGET || self.posters.len() >= POSTER_MAX {
			let Some(oldest) = self.poster_order.pop_front() else {
				break;
			};
			self.evict_poster(&oldest);
		}
		let image = egui::ColorImage::new([width, height], video_pixels(rgba).to_vec());
		self.posters.insert(
			id,
			(
				ctx.load_texture("inline-video-poster", image, egui::TextureOptions::LINEAR),
				bytes,
			),
		);
		self.poster_order.push_back(id);
		self.poster_bytes += bytes;
		self.poster_seen.insert(id);
		true
	}
	/// Drops one cached poster and its retry guard, freeing its budget share.
	fn evict_poster(&mut self, id: &Id) {
		if let Some((_, bytes)) = self.posters.remove(id) {
			self.poster_bytes = self.poster_bytes.saturating_sub(bytes);
			self.poster_order.retain(|queued| queued != id);
			self.poster_seen.remove(id);
		}
	}
	/// Marks one cached poster as recently used so repeat views keep it.
	fn touch_poster(&mut self, id: &Id) {
		if self.poster_order.back() != Some(id) && self.posters.contains_key(id) {
			self.poster_order.retain(|queued| queued != id);
			self.poster_order.push_back(*id);
		}
	}
	/// Records an undecodable poster so the stage is not requested again until eviction.
	pub fn poster_failed(&mut self, id: Id) {
		if self.poster_seen.len() >= POSTER_SEEN_MAX {
			let posters = &self.posters;
			self.poster_seen.retain(|seen| posters.contains_key(seen));
		}
		self.poster_seen.insert(id);
	}
	pub fn show(
		&mut self,
		ui: &mut egui::Ui,
		message: &Message,
		attachment: &Attachment,
		download: &mut crate::DownloadUi,
		opening: &mut Option<String>,
		demo: bool,
	) -> egui::Response {
		let original = attachment.media.url.clone();
		self.show_player(
			ui,
			message,
			attachment,
			false,
			original.as_deref(),
			false,
			download,
			opening,
			demo,
		)
	}
	/// A directly proxied embed video over an already painted poster.
	#[allow(clippy::too_many_arguments)]
	pub(crate) fn show_embedded(
		&mut self,
		ui: &mut egui::Ui,
		message: &Message,
		attachment: &Attachment,
		original: Option<&str>,
		poster: bool,
		download: &mut crate::DownloadUi,
		opening: &mut Option<String>,
		demo: bool,
	) -> egui::Response {
		self.show_player(
			ui, message, attachment, false, original, poster, download, opening, demo,
		)
	}
	#[allow(clippy::too_many_arguments)]
	fn show_player(
		&mut self,
		ui: &mut egui::Ui,
		message: &Message,
		attachment: &Attachment,
		fullscreen: bool,
		original: Option<&str>,
		poster: bool,
		download: &mut crate::DownloadUi,
		opening: &mut Option<String>,
		demo: bool,
	) -> egui::Response {
		let colors = crate::design::palette(ui);
		let active = self.active.as_ref().is_some_and(|(channel, id, file)| {
			*channel == message.channel && *id == message.id && file == attachment
		});
		let state = if active { self.state } else { VideoState::Idle };
		let width = ui.available_width().clamp(1.0, MAX_WIDTH);
		let size = if fullscreen {
			ui.available_size().max(egui::Vec2::splat(1.0))
		} else {
			stage_size(attachment, width)
		};
		let (stage, mut response) = ui.allocate_exact_size(size, egui::Sense::hover());
		if active && self.is_fullscreen() && !fullscreen {
			return response;
		}
		let label = crate::i18n::translate_if_key(match state {
			VideoState::Loading => "video-show-player-cancel",
			VideoState::Playing => "video-show-player-pause",
			VideoState::Paused => "video-show-player-resume",
			VideoState::Ended => "video-show-player-replay",
			VideoState::Failed(_) => "video-show-player-retry",
			VideoState::Idle => "video-show-player-play",
		});
		let painter = ui.painter().with_clip_rect(stage);
		if !poster || (active && self.texture.is_some()) {
			painter.rect_filled(stage, CORNER, egui::Color32::BLACK);
		}
		if !poster && self.texture.as_ref().filter(|_| active).is_none() {
			if let Some((poster, _)) = self.posters.get(&attachment.id) {
				let size = poster.resolution_vec2();
				if size.x > 0.0 && size.y > 0.0 {
					// Contain, like playback: the decoded aspect can differ
					// from the attachment metadata, and cover would paint
					// outside the allocated stage over nearby content.
					let scale = (stage.width() / size.x).min(stage.height() / size.y);
					let rect = egui::Rect::from_center_size(stage.center(), size * scale);
					egui::Image::new(egui::load::SizedTexture::new(poster.id(), rect.size()))
						.corner_radius(CORNER)
						.paint_at(ui, rect);
					self.touch_poster(&attachment.id);
				}
			} else if state == VideoState::Idle
				&& !self.poster_seen.contains(&attachment.id)
				&& attachment.size > 0
				&& attachment.size <= 100 * 1024 * 1024
				&& ui.is_rect_visible(stage)
				&& !self
					.poster_queue
					.iter()
					.any(|queued| queued.id == attachment.id)
			{
				// Queued, not commanded: playback clicks keep the command slot.
				self.poster_queue.push(attachment.clone());
			}
		}
		if let Some(texture) = self.texture.filter(|_| active) {
			let size = texture.size;
			let scale = (stage.width() / size.x).min(stage.height() / size.y);
			let image_rect = egui::Rect::from_center_size(stage.center(), size * scale);
			egui::Image::new(egui::load::SizedTexture::new(texture.id, image_rect.size()))
				.corner_radius(CORNER)
				.paint_at(ui, image_rect);
		}
		let hovered = response.hovered() || ui.rect_contains_pointer(stage);
		let show_controls = active
			&& state != VideoState::Idle
			&& (hovered || state != VideoState::Playing || self.controls_focused);
		// The controls are painted over the stage. Keep playback interaction out of both
		// overlay bands so a control click cannot also become a play/pause click.
		let action_top = (stage.top() + 44.0).min(stage.bottom());
		let action_bottom =
			(stage.bottom() - if show_controls { BAR_HEIGHT } else { 0.0 }).max(action_top);
		let action = ui.interact(
			egui::Rect::from_min_max(
				egui::pos2(stage.left(), action_top),
				egui::pos2(stage.right(), action_bottom),
			),
			response.id.with("playback"),
			egui::Sense::click(),
		);
		action.widget_info(|| {
			egui::WidgetInfo::labeled(
				egui::Role::Button,
				ui.is_enabled(),
				format!(
					"{label} {} {}",
					crate::i18n::translate("video-show-player-video"),
					attachment.filename
				),
			)
		});
		let center = if show_controls {
			stage.center() - egui::vec2(0.0, BAR_HEIGHT * 0.25)
		} else {
			stage.center()
		};
		let white = egui::Color32::WHITE;
		let show_center = match state {
			VideoState::Loading => {
				ui.put(
					egui::Rect::from_center_size(center, egui::Vec2::splat(36.0)),
					egui::Spinner::new().size(36.0).color(white),
				);
				false
			}
			VideoState::Playing => false,
			_ => true,
		};
		if show_center {
			let radius = 28.0;
			painter.circle_filled(
				center,
				radius,
				if hovered {
					egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200)
				} else {
					egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160)
				},
			);
			painter.circle_stroke(
				center,
				radius,
				egui::Stroke::new(1.5, egui::Color32::from_white_alpha(60)),
			);
			if matches!(state, VideoState::Ended | VideoState::Failed(_)) {
				crate::icons::paint(
					&painter,
					crate::icons::Icon::Reload,
					egui::Rect::from_center_size(center, egui::Vec2::splat(26.0)),
					white,
				);
			} else {
				painter.add(egui::Shape::convex_polygon(
					vec![
						center + egui::vec2(-8.0, -12.0),
						center + egui::vec2(13.0, 0.0),
						center + egui::vec2(-8.0, 12.0),
					],
					white,
					egui::Stroke::NONE,
				));
			}
		}
		if let VideoState::Failed(error) = state {
			let text_rect = egui::Rect::from_min_max(
				egui::pos2(stage.left() + 12.0, center.y + 38.0),
				egui::pos2(stage.right() - 12.0, stage.bottom()),
			);
			ui.scope_builder(
				egui::UiBuilder::new()
					.max_rect(text_rect)
					.layout(egui::Layout::top_down(egui::Align::Center)),
				|ui| {
					ui.add(
						egui::Label::new(
							egui::RichText::new(error)
								.size(12.0)
								.color(egui::Color32::from_white_alpha(230)),
						)
						.wrap(),
					)
					.on_hover_text(error);
				},
			);
		}
		if (show_controls && state != VideoState::Playing) || (!active && hovered) {
			let font = egui::FontId::proportional(12.0);
			let galley = painter.layout(
				attachment.filename.clone(),
				font,
				white,
				(stage.width() - 32.0).max(16.0),
			);
			let pill = egui::Rect::from_min_size(
				stage.left_top() + egui::vec2(8.0, 8.0),
				galley.size() + egui::vec2(12.0, 6.0),
			);
			painter.rect_filled(pill, 4, egui::Color32::from_black_alpha(150));
			painter.galley(pill.min + egui::vec2(6.0, 3.0), galley, white);
		}
		if state == VideoState::Idle
			&& let Some(duration) = attachment.duration_ms.filter(|ms| *ms > 0)
		{
			let galley = painter.layout_no_wrap(
				timestamp(duration as f64 / 1000.0),
				egui::FontId::proportional(12.0),
				white,
			);
			let badge = egui::Rect::from_min_size(
				egui::pos2(
					stage.left() + 8.0,
					stage.bottom() - 8.0 - galley.size().y - 6.0,
				),
				galley.size() + egui::vec2(12.0, 6.0),
			);
			painter.rect_filled(badge, 4, egui::Color32::from_black_alpha(150));
			painter.galley(badge.min + egui::vec2(6.0, 3.0), galley, white);
		}
		if action.clicked() {
			self.toggle(message, attachment, state);
		}
		response = action | response;
		let mut controls_focused = false;
		let context_click = ui.input(|i| {
			i.pointer.button_down(egui::PointerButton::Secondary)
				|| i.pointer.button_released(egui::PointerButton::Secondary)
		});
		if show_controls {
			let bar = egui::Rect::from_min_max(
				egui::pos2(stage.left(), stage.bottom() - BAR_HEIGHT),
				stage.right_bottom(),
			);
			let shade = self.shade(ui.ctx());
			egui::Image::new(egui::load::SizedTexture::new(shade, bar.size()))
				.corner_radius(egui::CornerRadius {
					nw: 0,
					ne: 0,
					sw: CORNER,
					se: CORNER,
				})
				.paint_at(ui, bar);
			let duration = if self.duration.is_finite() && self.duration > 0.0 {
				self.duration
			} else {
				1.0
			};
			let can_seek = self.duration.is_finite()
				&& self.duration > 0.0
				&& matches!(state, VideoState::Playing | VideoState::Paused);
			let id = response.id;
			// Seek: the full-width track along the top of the bar.
			let seek_rect = egui::Rect::from_min_max(
				egui::pos2(bar.left() + 12.0, bar.top() + 6.0),
				egui::pos2(bar.right() - 12.0, bar.top() + 22.0),
			);
			let seek = track(
				ui,
				seek_rect,
				id.with("seek"),
				(self.position.max(0.0), duration),
				5.0,
				can_seek,
				colors.accent,
				&crate::i18n::translate("video-show-player-seek-video"),
			);
			controls_focused |= seek.response.has_focus();
			if let Some(pointer) = seek.response.hover_pos().filter(|_| can_seek) {
				let at = f64::from((pointer.x - seek_rect.left()) / seek_rect.width())
					.clamp(0.0, 1.0)
					* duration;
				let galley = ui.painter().layout_no_wrap(
					timestamp(seek.preview.unwrap_or(at)),
					egui::FontId::proportional(11.0),
					white,
				);
				// Keep the tip inside the stage; one wider than a narrow stage centres on it.
				let half = galley.size().x / 2.0 + 10.0;
				let x = if stage.width() > 2.0 * half {
					pointer.x.clamp(stage.left() + half, stage.right() - half)
				} else {
					stage.center().x
				};
				let tip = egui::Rect::from_center_size(
					egui::pos2(x, seek_rect.top() - galley.size().y / 2.0 - 6.0),
					galley.size() + egui::vec2(12.0, 6.0),
				);
				ui.painter()
					.rect_filled(tip, 4, egui::Color32::from_black_alpha(200));
				ui.painter()
					.galley(tip.min + egui::vec2(6.0, 3.0), galley, white);
			}
			if let Some(position) = seek.commit
				&& !context_click
			{
				self.command = Some(VideoCommand::Seek(position));
				self.position = position;
			}
			response |= seek.response;
			// One row of round buttons below the track.
			let row = bar.top() + 38.0;
			let slot =
				|x: f32| egui::Rect::from_center_size(egui::pos2(x, row), egui::Vec2::splat(32.0));
			let play_rect = slot(bar.left() + 22.0);
			let play = round_button(
				ui,
				play_rect,
				id.with("toggle"),
				&label,
				colors.accent,
				|painter, rect, color| match state {
					VideoState::Playing => glyph_pause(painter, rect, color),
					VideoState::Paused | VideoState::Idle => glyph_play(painter, rect, color),
					VideoState::Loading => {
						crate::icons::paint(painter, crate::icons::Icon::Close, rect, color)
					}
					VideoState::Ended | VideoState::Failed(_) => {
						crate::icons::paint(painter, crate::icons::Icon::Reload, rect, color)
					}
				},
			);
			controls_focused |= play.has_focus();
			if play.clicked() {
				self.toggle(message, attachment, state);
			}
			response |= play;
			let muted = self.volume <= 0.0;
			let mute_label = crate::i18n::translate(if muted {
				"video-show-player-unmute"
			} else {
				"video-show-player-mute"
			});
			let mute_rect = slot(play_rect.right() + 20.0);
			let mute = round_button(
				ui,
				mute_rect,
				id.with("mute"),
				&mute_label,
				colors.accent,
				|painter, rect, color| {
					crate::icons::paint(painter, crate::icons::Icon::Speaker, rect, color);
					if muted {
						painter.line_segment(
							[
								rect.left_top() + egui::vec2(1.0, 1.0),
								rect.right_bottom() - egui::vec2(1.0, 1.0),
							],
							egui::Stroke::new(2.0, color),
						);
					}
				},
			);
			controls_focused |= mute.has_focus();
			if mute.clicked() && !context_click {
				self.volume = if muted {
					self.unmuted_volume
						.filter(|volume| *volume > 0.0)
						.unwrap_or(1.0)
				} else {
					self.unmuted_volume = Some(self.volume);
					0.0
				};
				self.command = Some(VideoCommand::Volume(self.volume));
			}
			response |= mute;
			let volume_width = (stage.width() * 0.16).clamp(40.0, 80.0);
			let volume_rect = egui::Rect::from_min_size(
				egui::pos2(mute_rect.right() + 2.0, row - 8.0),
				egui::vec2(volume_width, 16.0),
			);
			let volume = track(
				ui,
				volume_rect,
				id.with("volume"),
				(f64::from(self.volume), 1.0),
				0.05,
				true,
				white,
				"Video volume",
			);
			controls_focused |= volume.response.has_focus();
			if let Some(value) = volume.preview.or(volume.commit)
				&& !context_click
			{
				self.volume = value as f32;
				self.command = Some(VideoCommand::Volume(self.volume));
			}
			response |= volume
				.response
				.on_hover_text(crate::i18n::translate("video-show-player-video-volume"));
			let screen_label = if fullscreen {
				"Exit fullscreen (Esc)"
			} else {
				"Fullscreen"
			};
			let screen_rect = slot(bar.right() - 22.0);
			let screen = round_button(
				ui,
				screen_rect,
				id.with("fullscreen"),
				screen_label,
				colors.accent,
				|painter, rect, color| {
					if fullscreen {
						glyph_exit_fullscreen(painter, rect, color);
					} else {
						crate::icons::paint(painter, crate::icons::Icon::Fullscreen, rect, color);
					}
				},
			);
			controls_focused |= screen.has_focus();
			let screen_id = screen.id;
			if screen.clicked() {
				if fullscreen {
					self.exit_fullscreen();
				} else {
					let previous = ui.input(|i| i.viewport().fullscreen.unwrap_or(false));
					self.fullscreen = Some((ui.ctx().clone(), previous, screen_id));
					self.fullscreen_request = Some(true);
				}
			}
			response |= screen;
			// Elapsed and total time between the volume and fullscreen controls; the total
			// is dropped first when the bar is narrow.
			let position = seek.preview.unwrap_or(self.position);
			let total = if self.duration > 0.0 {
				timestamp(self.duration)
			} else {
				"--:--".into()
			};
			let room = screen_rect.left() - volume_rect.right() - 20.0;
			let font = egui::FontId::proportional(12.0);
			let full = ui.painter().layout_no_wrap(
				format!("{} / {total}", timestamp(position)),
				font.clone(),
				white,
			);
			let galley = if full.size().x <= room {
				full
			} else {
				ui.painter()
					.layout_no_wrap(timestamp(position), font, white)
			};
			if galley.size().x <= room {
				ui.painter().galley(
					egui::pos2(volume_rect.right() + 12.0, row - galley.size().y / 2.0),
					galley,
					white,
				);
			}
		}
		self.controls_focused = controls_focused;
		// Download/open controls stay visible regardless of load state, unlike the bottom bar.
		// Added last so Tab order still reaches the playback controls first.
		let overlay = egui::Rect::from_min_size(
			egui::pos2(stage.left() + 8.0, stage.top() + 8.0),
			egui::vec2((stage.width() - 16.0).max(0.0), 28.0),
		);
		ui.scope_builder(
			egui::UiBuilder::new()
				.max_rect(overlay)
				.layout(egui::Layout::right_to_left(egui::Align::Min)),
			|ui| {
				ui.spacing_mut().item_spacing.x = 6.0;
				let idle = !demo && !download.busy();
				if !is_embedded(attachment)
					&& ui
						.add_enabled_ui(idle, |ui| {
							crate::attachments::glass_button(
								ui,
								crate::icons::Icon::Download,
								28.0,
								"Download",
							)
						})
						.inner
						.on_disabled_hover_text(crate::i18n::translate_if_key(if demo {
							"video-show-player-downloads-are-disabled-for-synthetic-attachments"
						} else {
							"video-show-player-a-download-is-already-active"
						}))
						.clicked()
				{
					download.request = Some(attachment.clone());
				}
				if let Some(url) = original.and_then(crate::markdown::external_url)
					&& crate::attachments::glass_button(
						ui,
						crate::icons::Icon::External,
						28.0,
						"Open original…",
					)
					.clicked()
				{
					*opening = Some(url);
				}
			},
		);
		if response.has_focus() {
			ui.painter().rect_stroke(
				stage,
				CORNER,
				egui::Stroke::new(2.0, colors.accent),
				egui::StrokeKind::Inside,
			);
		}
		if ui.is_rect_visible(stage)
			&& self.active.as_ref().is_some_and(|(channel, id, file)| {
				*channel == message.channel && *id == message.id && file == attachment
			}) {
			self.seen = true;
		}
		response
	}
}
impl VideoUi {
	/// A provider embed's poster; playing shows the video in this embed box, with theater and
	/// mini-player states a click away.
	#[allow(clippy::too_many_arguments)]
	pub(crate) fn show_provider(
		&mut self,
		ui: &mut egui::Ui,
		message: &Message,
		selected: Option<Id>,
		video: &WebVideo,
		poster: Option<&model::EmbedMedia>,
		images: &mut crate::avatars::Avatars,
		opening: &mut Option<String>,
		demo: bool,
	) {
		let size = provider_stage(video, ui.available_width());
		let (stage, _) = ui.allocate_exact_size(size, egui::Sense::hover());
		// This message's embed owns inline playback: the webview covers everything below
		// the strip, so the strip carries the expand and stop controls.
		let playing_here = self.web_mode == WebMode::Inline
			&& self.web.as_ref().is_some_and(|open| open == video)
			&& self
				.web_source
				.is_some_and(|(open_selected, channel, id, _)| {
					open_selected == selected && channel == message.channel && id == message.id
				});
		// A provider webview is transparent around its own card. Painting the poster
		// underneath would leak the thumbnail through rounded corners and margins, so
		// playback sits on a solid stage instead.
		if playing_here {
			ui.painter()
				.rect_filled(stage, CORNER, egui::Color32::BLACK);
		} else if let Some(poster) = poster {
			let mut image_ui = ui.new_child(egui::UiBuilder::new().max_rect(stage));
			images.show_media(
				&mut image_ui,
				poster,
				size,
				demo,
				crate::avatars::Surface::Banner,
			);
		} else {
			ui.painter()
				.rect_filled(stage, CORNER, egui::Color32::BLACK);
		}
		if playing_here {
			const HEADER: f32 = 30.0;
			let bar = egui::Rect::from_min_max(
				stage.min,
				egui::pos2(stage.max.x, (stage.min.y + HEADER).min(stage.max.y)),
			);
			// A message scrolled halfway out hides the webview instead of clipping it.
			let view = egui::Rect::from_min_max(bar.left_bottom(), stage.max);
			let clipped = view.intersect(ui.clip_rect());
			self.web_bounds = clipped.is_positive().then_some(clipped);
			ui.painter().rect_filled(
				bar,
				egui::CornerRadius {
					nw: CORNER,
					ne: CORNER,
					sw: 0,
					se: 0,
				},
				egui::Color32::from_black_alpha(235),
			);
			let mut controls = ui.new_child(
				egui::UiBuilder::new()
					.max_rect(bar)
					.layout(egui::Layout::right_to_left(egui::Align::Center)),
			);
			controls.spacing_mut().item_spacing.x = 6.0;
			controls.add_space(6.0);
			if crate::attachments::glass_button(
				&mut controls,
				crate::icons::Icon::Close,
				24.0,
				"Stop playback",
			)
			.clicked()
			{
				self.web_stop();
			}
			if crate::attachments::glass_button(
				&mut controls,
				crate::icons::Icon::ChevronDown,
				24.0,
				"Mini player",
			)
			.clicked()
			{
				self.web_mode = WebMode::Mini;
			}
			// Providers can refuse a post inside the embed (X does for age-restricted
			// media); keep the shared link reachable while the player is covering it.
			if let Some(page) = &video.page
				&& crate::attachments::glass_button(
					&mut controls,
					crate::icons::Icon::External,
					24.0,
					"Open in browser…",
				)
				.clicked()
			{
				*opening = Some(page.clone());
			}
			controls.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
				ui.add(
					egui::Label::new(
						egui::RichText::new(if video.title.is_empty() {
							&video.provider
						} else {
							&video.title
						})
						.strong()
						.color(egui::Color32::WHITE),
					)
					.truncate(),
				);
			});
			return;
		}
		let play = ui.interact(
			stage.shrink2(egui::vec2(0.0, 44.0).min(stage.size() / 3.0)),
			ui.scope_id().with(("provider-play", &video.player)),
			egui::Sense::click(),
		);
		let label = format!("Play {} video {}", video.provider, video.title);
		play.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), &label));
		let hovered = ui.rect_contains_pointer(stage);
		let painter = ui.painter().with_clip_rect(stage);
		let center = stage.center();
		painter.circle_filled(
			center,
			28.0,
			egui::Color32::from_black_alpha(if hovered { 200 } else { 160 }),
		);
		painter.circle_stroke(
			center,
			28.0,
			egui::Stroke::new(1.5, egui::Color32::from_white_alpha(60)),
		);
		painter.add(egui::Shape::convex_polygon(
			vec![
				center + egui::vec2(-8.0, -12.0),
				center + egui::vec2(13.0, 0.0),
				center + egui::vec2(-8.0, 12.0),
			],
			egui::Color32::WHITE,
			egui::Stroke::NONE,
		));
		if play.has_focus() {
			painter.rect_stroke(
				stage,
				CORNER,
				egui::Stroke::new(2.0, crate::design::palette(ui).accent),
				egui::StrokeKind::Inside,
			);
		}
		if let Some(page) = &video.page {
			let mut controls = ui.new_child(
				egui::UiBuilder::new()
					.max_rect(egui::Rect::from_min_size(
						egui::pos2(stage.left() + 8.0, stage.top() + 8.0),
						egui::vec2((stage.width() - 16.0).max(0.0), 28.0),
					))
					.layout(egui::Layout::right_to_left(egui::Align::Min)),
			);
			if crate::attachments::glass_button(
				&mut controls,
				crate::icons::Icon::External,
				28.0,
				"Open in browser…",
			)
			.clicked()
			{
				*opening = Some(page.clone());
			}
		}
		if play.on_hover_text("Play here").clicked() {
			// Playing in the embed box; theater and mini stay one click away from it.
			if self.web.as_ref() == Some(video) {
				self.web_mode = WebMode::Inline;
			} else {
				self.stop();
				self.web = Some(video.clone());
				self.web_source = Some((
					selected,
					message.channel,
					message.id,
					egui::Id::unique((&message.embeds, &message.attachments)),
				));
				self.web_notice = None;
				self.web_mode = WebMode::Inline;
			}
		}
	}

	/// Stops the provider player entirely; the mini player keeps its remembered geometry.
	fn web_stop(&mut self) {
		self.web = None;
		self.web_source = None;
		self.web_bounds = None;
		self.web_notice = None;
		self.web_mode = WebMode::default();
	}
	pub(super) fn web_source_available(&self, state: &client_core::State) -> bool {
		let Some((selected, channel, id, media)) = self.web_source else {
			return false;
		};
		if selected != state.selected
			|| !state.can_read_history(channel)
			|| state.timeline.is_deleted(id)
		{
			return false;
		}
		if let Some(message) = state.timeline.get(id).or_else(|| {
			state
				.interactions
				.ephemeral
				.iter()
				.find(|message| message.id == id)
		}) {
			return message.channel == channel
				&& !message.embeds_suppressed
				&& media == egui::Id::unique((&message.embeds, &message.attachments));
		}
		state
			.search
			.as_ref()
			.and_then(|search| search.page.as_ref())
			.is_some_and(|page| {
				page.hits.iter().any(|hit| {
					hit.id == id
						&& hit.channel == channel
						&& media == egui::Id::unique((&hit.embeds, &hit.attachments))
				})
			})
	}
	/// The provider player presentation. The desktop places the webview over the
	/// Stage reserved here, so nothing else may paint above it while it is open.
	pub(super) fn show_web_player(
		&mut self,
		ctx: &egui::Context,
		opening: &mut Option<String>,
		composer: Option<egui::Rect>,
	) {
		if let Some(url) = self.web_external.take() {
			*opening = crate::markdown::external_url(&url);
			self.web_stop();
		}
		let Some(video) = self.web.clone() else {
			self.web_source = None;
			self.web_bounds = None;
			self.web_mode = WebMode::default();
			return;
		};
		match self.web_mode {
			WebMode::Inline => {
				// The embed stage publishes its on-screen rect while drawing; a source
				// scrolled away hides the webview and keeps the audio playing.
				self.web_bounds = None;
			}
			WebMode::Mini => self.show_mini_player(ctx, &video, composer, opening),
		}
	}

	/// The floating mini player: header strip and grip around the webview video over the
	/// bottom-left of the conversation, so typing and the stream PiP stay usable.
	fn show_mini_player(
		&mut self,
		ctx: &egui::Context,
		video: &WebVideo,
		composer: Option<egui::Rect>,
		opening: &mut Option<String>,
	) {
		// A modal above must not have a native webview painted over it; audio keeps playing.
		if ctx.memory(|memory| memory.top_modal_layer().is_some()) {
			self.web_bounds = None;
			return;
		}
		const HEADER: f32 = 30.0;
		const EDGE: f32 = 10.0;
		const MARGIN: f32 = 18.0;
		const MIN_VIDEO: egui::Vec2 = egui::vec2(280.0, 158.0);
		let screen = ctx.content_rect();
		let aspect = if video.width > 0 && video.height > 0 {
			(video.width as f32 / video.height as f32).clamp(0.5, 2.4)
		} else {
			16.0 / 9.0
		};
		let size = self
			.web_mini_size
			.unwrap_or_else(|| {
				let width = 400.0_f32.min(screen.width() * 0.5);
				egui::vec2(width, (width / aspect).max(MIN_VIDEO.y))
			})
			.max(MIN_VIDEO)
			.min(screen.size() - egui::vec2(2.0 * MARGIN, 2.0 * MARGIN + HEADER + EDGE));
		// Default spot: bottom-left, lifted above the composer so typing stays clear.
		let composer_top = composer.map_or(screen.bottom() - 104.0, |rect| rect.top());
		let default_pos = egui::pos2(
			screen.left() + MARGIN,
			(composer_top - MARGIN - HEADER - EDGE - size.y).max(screen.top() + MARGIN),
		);
		let pos = self.web_mini_position.unwrap_or(default_pos);
		let pos = egui::pos2(
			pos.x.clamp(
				screen.left() + MARGIN,
				(screen.right() - size.x - MARGIN).max(screen.left() + MARGIN),
			),
			pos.y.clamp(
				screen.top() + MARGIN,
				(screen.bottom() - HEADER - EDGE - MARGIN).max(screen.top() + MARGIN),
			),
		);
		self.web_mini_position = Some(pos);
		let area = egui::Rect::from_min_size(pos, egui::vec2(size.x, HEADER + size.y + EDGE));
		egui::Area::new(egui::Id::unique("web-video-mini"))
			.order(egui::Order::Foreground)
			.fixed_pos(pos)
			.show(ctx, |ui| {
				let (rect, _) = ui.allocate_exact_size(area.size(), egui::Sense::hover());
				ui.painter()
					.rect_filled(rect, CORNER, egui::Color32::from_black_alpha(235));
				// The header is outside the webview rect, so it stays clickable: drag it to
				// move the player, use its buttons without the page stealing the click.
				let handle = ui.interact(
					egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + HEADER)),
					ui.scope_id().with("web-mini-drag"),
					egui::Sense::click_and_drag(),
				);
				if handle.dragged() {
					self.web_mini_position = Some(pos + handle.drag_delta());
				}
				handle.widget_info(|| {
					egui::WidgetInfo::labeled(egui::Role::Button, true, "Move video player")
				});
				let mut header = ui.new_child(
					egui::UiBuilder::new()
						.max_rect(egui::Rect::from_min_max(
							rect.min,
							egui::pos2(rect.max.x, rect.min.y + HEADER),
						))
						.layout(egui::Layout::right_to_left(egui::Align::Center)),
				);
				header.spacing_mut().item_spacing.x = 6.0;
				header.add_space(6.0);
				if crate::attachments::glass_button(
					&mut header,
					crate::icons::Icon::Close,
					24.0,
					"Stop playback",
				)
				.clicked()
				{
					self.web_stop();
				}
				if let Some(page) = &video.page
					&& crate::attachments::glass_button(
						&mut header,
						crate::icons::Icon::External,
						24.0,
						"Open in browser…",
					)
					.clicked()
				{
					*opening = Some(page.clone());
					self.web_stop();
				}
				header.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
					ui.add(
						egui::Label::new(
							egui::RichText::new(if video.title.is_empty() {
								&video.provider
							} else {
								&video.title
							})
							.strong()
							.color(egui::Color32::WHITE),
						)
						.truncate(),
					);
				});
				// The grip lives on the edge strip, also outside the webview rect.
				let grip = ui.interact(
					egui::Rect::from_min_size(
						rect.right_bottom() - egui::vec2(24.0, EDGE),
						egui::vec2(24.0, EDGE),
					),
					ui.scope_id().with("web-mini-resize"),
					egui::Sense::drag(),
				);
				if grip.hovered() {
					ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
				}
				if grip.dragged() {
					self.web_mini_size = Some((size + grip.drag_delta()).max(MIN_VIDEO).min(
						screen.size() - egui::vec2(2.0 * MARGIN, 2.0 * MARGIN + HEADER + EDGE),
					));
				}
				// The webview covers only the video area between the chrome strips.
				let view = egui::Rect::from_min_max(
					egui::pos2(rect.left(), rect.min.y + HEADER),
					egui::pos2(rect.right(), rect.bottom() - EDGE),
				);
				if let Some(notice) = self.web_notice {
					ui.painter().text(
						view.center(),
						egui::Align2::CENTER_CENTER,
						notice,
						egui::FontId::proportional(12.0),
						egui::Color32::from_white_alpha(220),
					);
				}
				self.web_bounds = Some(view);
			});
	}
}
impl Drop for VideoUi {
	fn drop(&mut self) {
		self.exit_fullscreen();
	}
}
pub(crate) fn is_embedded(attachment: &Attachment) -> bool {
	attachment.size == 0
}
pub(crate) fn provider_stage(video: &WebVideo, width: f32) -> egui::Vec2 {
	let (w, h) = if video.width > 0 && video.height > 0 {
		(video.width as f32, video.height as f32)
	} else {
		(16.0, 9.0)
	};
	let width = width.clamp(1.0, MAX_WIDTH);
	if video.provider == "X" {
		// X renders portrait media no taller than it is wide; reserve that square plus
		// the post chrome so the clip is visible without scrolling inside the webview.
		let media = (width * h / w).min(width);
		return egui::vec2(width, media + X_CHROME_HEIGHT).max(egui::vec2(1.0, 1.0));
	}
	let scale = (width / w).min(MAX_HEIGHT / h);
	(egui::vec2(w, h) * scale).max(egui::vec2(1.0, 1.0))
}
pub(crate) fn embed_stage_height(width: u32, height: u32, available: f32) -> f32 {
	let (w, h) = if width > 0 && height > 0 {
		(width as f32, height as f32)
	} else {
		(16.0, 9.0)
	};
	let available = available.clamp(1.0, MAX_WIDTH);
	h * (available / w).min(MAX_HEIGHT / h)
}
pub(crate) fn stage_size(attachment: &Attachment, width: f32) -> egui::Vec2 {
	let width = width.clamp(1.0, MAX_WIDTH);
	let (native_width, native_height) = if attachment.media.width > 0 && attachment.media.height > 0
	{
		(
			attachment.media.width as f32,
			attachment.media.height as f32,
		)
	} else {
		(16.0, 9.0)
	};
	let scale = (width / native_width)
		.min(MAX_HEIGHT / native_height)
		.min(1.0);
	(egui::vec2(native_width, native_height) * scale).max(egui::vec2(1.0, 1.0))
}
/// Stage plus the spacing after each attachment; all controls are overlays or menu actions.
pub(super) fn estimated_height(attachment: &Attachment, width: f32) -> f32 {
	stage_size(attachment, width.clamp(1.0, MAX_WIDTH)).y + 6.0
}
/// A seek or volume track: thin and rounded, thicker with a knob while hovered, dragged or
/// focused. Dragging previews; release, a click or an arrow key commits.
struct Track {
	response: egui::Response,
	preview: Option<f64>,
	commit: Option<f64>,
}

#[allow(clippy::too_many_arguments)]
fn track(
	ui: &mut egui::Ui,
	rect: egui::Rect,
	id: egui::Id,
	(value, max): (f64, f64),
	step: f64,
	enabled: bool,
	fill: egui::Color32,
	label: &str,
) -> Track {
	let response = ui.interact(
		rect,
		id,
		if enabled {
			egui::Sense::click_and_drag()
		} else {
			egui::Sense::hover()
		},
	);
	response.widget_info(|| egui::WidgetInfo::slider(enabled, value, label));
	ui.ctx().accesskit_node_builder(id, |node| {
		use egui::accesskit::Action;
		node.set_min_numeric_value(0.0);
		node.set_max_numeric_value(max);
		node.set_numeric_value_step(step);
		if enabled {
			node.add_action(Action::SetValue);
			if value < max {
				node.add_action(Action::Increment);
			}
			if value > 0.0 {
				node.add_action(Action::Decrement);
			}
		}
	});
	// A stage narrower than the bar's insets leaves no track; never divide by its width.
	let at = |x: f32| f64::from((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) * max;
	let mut preview = None;
	let mut commit = None;
	if enabled {
		if response.has_focus() {
			ui.memory_mut(|memory| {
				memory.set_focus_lock_filter(
					id,
					egui::EventFilter {
						horizontal_arrows: true,
						..Default::default()
					},
				)
			});
		}
		// Arrow keys and a screen reader's increment, decrement or set-value requests, as
		// `egui::Slider` handles them.
		let focused = response.has_focus();
		let (delta, set) = ui.input(|i| {
			use egui::accesskit::{Action, ActionData};
			let keys = if focused {
				f64::from(i8::from(i.key_pressed(egui::Key::ArrowRight)))
					- f64::from(i8::from(i.key_pressed(egui::Key::ArrowLeft)))
			} else {
				0.0
			};
			let requested = i.num_accesskit_action_requests(id, Action::Increment) as f64
				- i.num_accesskit_action_requests(id, Action::Decrement) as f64;
			let set = i
				.accesskit_action_requests(id, Action::SetValue)
				.filter_map(|request| match request.data {
					Some(ActionData::NumericValue(value)) => Some(value),
					_ => None,
				})
				.last();
			(keys + requested, set)
		});
		if let Some(value) = set.filter(|value| value.is_finite()) {
			commit = Some(value.clamp(0.0, max));
		} else if delta != 0.0 {
			commit = Some((value + delta * step).clamp(0.0, max));
		}
		let pointer = response
			.interact_pointer_pos()
			.or_else(|| ui.input(|i| i.pointer.interact_pos()));
		if let Some(pointer) = pointer {
			if response.is_pointer_button_down_on() || response.dragged() {
				preview = Some(at(pointer.x));
			}
			if response.drag_stopped() || response.clicked() {
				commit = Some(at(pointer.x));
			}
		}
	}
	let hot = enabled && (response.hovered() || response.dragged() || response.has_focus());
	let shown = preview.or(commit).unwrap_or(value).clamp(0.0, max);
	let thickness = if hot { 6.0 } else { 4.0 };
	let line = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), thickness));
	let radius = (thickness / 2.0) as u8;
	let painter = ui.painter();
	painter.rect_filled(
		line,
		radius,
		egui::Color32::from_white_alpha(if enabled { 64 } else { 32 }),
	);
	let knob = line.left() + line.width() * (shown / max.max(f64::EPSILON)) as f32;
	painter.rect_filled(
		egui::Rect::from_min_max(line.min, egui::pos2(knob, line.bottom())),
		radius,
		if enabled {
			fill
		} else {
			fill.gamma_multiply(0.5)
		},
	);
	if hot {
		painter.circle_filled(egui::pos2(knob, line.center().y), 6.0, egui::Color32::WHITE);
	}
	if response.has_focus() {
		painter.rect_stroke(
			rect.expand(2.0),
			4,
			egui::Stroke::new(2.0, fill),
			egui::StrokeKind::Outside,
		);
	}
	if enabled && response.hovered() {
		ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
	}

	Track {
		response,
		preview,
		commit,
	}
}

/// A round overlay button: a soft disc on hover and an accent ring when focused.
fn round_button(
	ui: &mut egui::Ui,
	rect: egui::Rect,
	id: egui::Id,
	label: &str,
	focus: egui::Color32,
	glyph: impl FnOnce(&egui::Painter, egui::Rect, egui::Color32),
) -> egui::Response {
	let response = ui.interact(rect, id, egui::Sense::click());
	response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), label));
	let hot = response.hovered() || response.has_focus();
	let painter = ui.painter();
	if hot {
		painter.circle_filled(
			rect.center(),
			rect.width() / 2.0,
			egui::Color32::from_white_alpha(36),
		);
	}
	if response.has_focus() {
		painter.circle_stroke(
			rect.center(),
			rect.width() / 2.0,
			egui::Stroke::new(2.0, focus),
		);
	}
	glyph(
		painter,
		egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(18.0)),
		if hot {
			egui::Color32::WHITE
		} else {
			egui::Color32::from_white_alpha(225)
		},
	);
	response
		.on_hover_text(label)
		.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn glyph_play(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
	let (left, right) = (
		rect.left() + rect.width() * 0.2,
		rect.right() - rect.width() * 0.05,
	);
	painter.add(egui::Shape::convex_polygon(
		vec![
			egui::pos2(left, rect.top() + 1.0),
			egui::pos2(right, rect.center().y),
			egui::pos2(left, rect.bottom() - 1.0),
		],
		color,
		egui::Stroke::new(1.5, color),
	));
}

fn glyph_pause(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
	for x in [-0.22, 0.22] {
		painter.rect_filled(
			egui::Rect::from_center_size(
				rect.center() + egui::vec2(rect.width() * x, 0.0),
				egui::vec2(rect.width() * 0.22, rect.height() * 0.85),
			),
			1,
			color,
		);
	}
}

/// Four corners turned inward, the reverse of the fullscreen icon.
fn glyph_exit_fullscreen(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
	let stroke = egui::Stroke::new(2.0, color);
	let arm = rect.width() * 0.3;
	let inset = rect.shrink(rect.width() * 0.1);
	for (corner, sx, sy) in [
		(inset.left_top(), 1.0, 1.0),
		(inset.right_top(), -1.0, 1.0),
		(inset.left_bottom(), 1.0, -1.0),
		(inset.right_bottom(), -1.0, -1.0),
	] {
		let elbow = corner + egui::vec2(sx * arm, sy * arm);
		painter.line_segment([egui::pos2(corner.x, elbow.y), elbow], stroke);
		painter.line_segment([egui::pos2(elbow.x, corner.y), elbow], stroke);
	}
}

fn timestamp(seconds: f64) -> String {
	let seconds = seconds.max(0.0) as u64;
	format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
	use super::*;
	fn provider_message(channel: Id) -> Message {
		let mut message = test_support::message(7001, channel);
		message.embeds = vec![model::Embed {
			kind: "video".into(),
			title: Some("Synthetic provider clip".into()),
			url: Some("https://www.youtube.com/watch?v=KwRSAfoW5uo".into()),
			video: Some(model::EmbedMedia {
				url: Some("https://www.youtube.com/embed/KwRSAfoW5uo".into()),
				width: 1280,
				height: 720,
				..Default::default()
			}),
			..Default::default()
		}];
		message
	}
	#[test]
	fn provider_poster_and_controls_share_one_stage() {
		let message = provider_message(Id(2));
		let web = crate::embeds::provider_video(&message.embeds[0]).unwrap();
		let poster = model::EmbedMedia {
			width: 1280,
			height: 720,
			..Default::default()
		};
		for poster in [None, Some(&poster)] {
			let ctx = egui::Context::default();
			let mut video = VideoUi::default();
			ctx.run_ui(egui::RawInput::default(), |ui| {
				ui.set_width(320.0);
				let stage = egui::Rect::from_min_size(
					ui.cursor().min,
					provider_stage(&web, ui.available_width()),
				);
				let play = ui.scope_id().with(("provider-play", &web.player));
				video.show_provider(
					ui,
					&message,
					Some(message.channel),
					&web,
					poster,
					&mut crate::avatars::Avatars::default(),
					&mut None,
					true,
				);
				assert!((ui.min_rect().height() - stage.height()).abs() < 1.0);
				assert!(
					ui.cursor().top() >= stage.bottom(),
					"overlay controls rewound the layout cursor"
				);
				assert!(
					ctx.read_response(play)
						.unwrap()
						.rect
						.contains(stage.center()),
					"play button missed its poster"
				);
			})
			.drop_without_applying_deltas();
		}
	}
	#[test]
	fn provider_web_player_survives_only_while_its_source_is_available() {
		let mut state = test_support::demo_state();
		let selected = state.selected;
		let channel = selected.unwrap();
		let mut message = provider_message(channel);
		let web = crate::embeds::provider_video(&message.embeds[0]).unwrap();
		state.timeline.insert(message.clone(), true, false).unwrap();
		let ctx = egui::Context::default();
		let mut view = crate::timeline::TimelineView::default();
		let source = (
			selected,
			channel,
			message.id,
			egui::Id::unique((&message.embeds, &message.attachments)),
		);
		let mut frame = |state: &client_core::State, available: bool| {
			view.video.web = Some(web.clone());
			view.video.web_source = Some(source);
			view.video.web_mode = crate::video::WebMode::Mini;
			ctx.run_ui(egui::RawInput::default(), |ui| {
				// The inline and mini presentations never own the client surface.
				assert!(!view.show_fullscreen_video(ui.ctx(), state, true));
			})
			.drop_without_applying_deltas();
			assert_eq!(view.video.web.is_some(), available);
			assert_eq!(view.video.web_bounds.is_some(), available);
		};
		frame(&state, true);
		state.selected = None;
		frame(&state, false);
		state.selected = selected;
		message.embeds_suppressed = true;
		state.timeline.insert(message.clone(), true, false).unwrap();
		frame(&state, false);
		message.embeds_suppressed = false;
		message.embeds[0].title = Some("||New spoiler||".into());
		state.timeline.insert(message.clone(), true, false).unwrap();
		frame(&state, false);
		message = provider_message(channel);
		state.timeline.insert(message.clone(), true, false).unwrap();
		assert!(state.request_search("synthetic".into(), None).is_some());
		state.search.as_mut().unwrap().page = Some(model::SearchPage {
			hits: vec![model::SearchHit {
				id: message.id,
				channel,
				author: message.author.clone(),
				mentions: vec![],
				excerpt: String::new(),
				attachments: message.attachments.clone(),
				embeds: message.embeds.clone(),
			}],
			total: 1,
			partial: false,
			pin_cursor: None,
		});
		state.timeline.clear();
		frame(&state, true);
		state.search = None;
		frame(&state, false);
		state.timeline.insert(message, true, false).unwrap();
		state.permissions = Default::default();
		frame(&state, false);
	}
	#[test]
	fn video_controls_are_explicit_bounded_and_keyboard_operable() {
		let mut message = test_support::message(1, Id(2));
		let attachment = Attachment {
			id: Id(3),
			filename: "synthetic-clip.MOV".into(),
			description: None,
			content_type: Some("application/octet-stream".into()),
			size: 128,
			media: model::EmbedMedia {
				width: 1920,
				height: 1080,
				..Default::default()
			},
			spoiler: false,
			duration_ms: None,
			waveform: Vec::new(),
		};
		message.attachments.push(attachment.clone());
		for (width, theme) in [(220.0, egui::Theme::Dark), (420.0, egui::Theme::Light)] {
			let ctx = egui::Context::default();
			crate::design::apply(&ctx);
			ctx.set_theme(theme);
			let mut video = VideoUi::default();
			let frame = |video: &mut VideoUi, key: Option<egui::Key>| {
				ctx.run_ui(
					egui::RawInput {
						screen_rect: Some(egui::Rect::from_min_size(
							egui::Pos2::ZERO,
							egui::vec2(width + 16.0, 600.0),
						)),
						events: key
							.into_iter()
							.map(|key| egui::Event::Key {
								key,
								physical_key: None,
								pressed: true,
								repeat: false,
								modifiers: egui::Modifiers::NONE,
							})
							.collect(),
						..Default::default()
					},
					|ui| {
						ui.set_width(width);
						crate::attachments::show(
							ui,
							&message,
							&mut crate::avatars::Avatars::default(),
							&mut None,
							&mut None,
							&mut crate::attachments::DownloadUi::default(),
							&mut crate::AudioUi::default(),
							video,
							false,
							&mut crate::select::Surface::new(ui, "attachment-test"),
							crate::design::MessageCardSurface::Conversation,
						);
						assert!(ui.min_rect().width() <= width + 2.0);
						// Only the stage and attachment spacing drive the layout estimate;
						// overlay controls and menu actions never add height.
						assert!(
							(ui.min_rect().height() - estimated_height(&attachment, width)).abs()
								< 24.0,
							"{} vs {}",
							ui.min_rect().height(),
							estimated_height(&attachment, width)
						);
					},
				)
				.drop_without_applying_deltas();
			};
			frame(&mut video, None);
			assert!(video.command.is_none() && video.active.is_none());
			assert!(!video.accept_frame(&ctx, 1, 1, &[0, 0, 0, 255]));
			for key in [egui::Key::Tab, egui::Key::Enter] {
				frame(&mut video, Some(key));
			}
			assert!(
				matches!(video.command.take(), Some(VideoCommand::Play(file)) if file == attachment)
			);
			assert!(video.seen);
			assert!(!video.accept_frame(&ctx, 3841, 1080, &[]));
			assert!(!video.accept_frame(&ctx, 1, 3841, &[]));
			assert!(!video.accept_frame(&ctx, 3840, 3840, &[]));
			assert!(!video.accept_frame(&ctx, usize::MAX, usize::MAX, &[]));
			assert!(!video.accept_frame(&ctx, 1, 1, &[0]));
			// Within the pixel bound, but taller than this renderer's largest texture.
			assert!(!video.accept_frame(&ctx, 1, 3840, &vec![0; 3840 * 4]));
			assert!(video.accept_frame(&ctx, 1, 2048, &vec![0; 2048 * 4]));
			assert!(video.accept_frame(&ctx, 1, 1, &[0, 0, 0, 255]));
			video.state = VideoState::Playing;
			video.duration = 12.0;
			frame(&mut video, Some(egui::Key::Enter));
			assert!(matches!(
				video.command.take(),
				Some(VideoCommand::Pause(true))
			));
			video.state = VideoState::Paused;
			frame(&mut video, Some(egui::Key::Enter));
			assert!(matches!(
				video.command.take(),
				Some(VideoCommand::Pause(false))
			));
			for key in [egui::Key::Tab, egui::Key::ArrowRight] {
				frame(&mut video, Some(key));
			}
			assert!(matches!(video.command.take(), Some(VideoCommand::Seek(value)) if value > 0.0));
			// While a control keeps keyboard focus, playback resuming must not hide the overlay.
			video.state = VideoState::Playing;
			frame(&mut video, Some(egui::Key::ArrowRight));
			assert!(matches!(video.command.take(), Some(VideoCommand::Seek(_))));
			// Seek, then play/pause and mute, then the volume track.
			for key in [
				egui::Key::Tab,
				egui::Key::Tab,
				egui::Key::Tab,
				egui::Key::ArrowLeft,
			] {
				frame(&mut video, Some(key));
			}
			assert!(
				matches!(video.command.take(), Some(VideoCommand::Volume(value)) if value < 1.0)
			);
			video.state = VideoState::Failed("Unsupported video codec");
			frame(&mut video, None);
			video.stop();
			assert!(video.active.is_none() && video.texture.is_none());
			assert!(matches!(video.command, Some(VideoCommand::Stop)));
		}
	}
	/// Idle stages queue one poster, cache the decoded answer, and evict oldest-first.
	#[test]
	fn idle_stage_requests_and_paints_one_decoded_poster() {
		let ctx = egui::Context::default();
		crate::design::apply(&ctx);
		let message = test_support::message(1, Id(2));
		let attachment = Attachment {
			id: Id(3),
			filename: "clip.mp4".into(),
			description: None,
			content_type: Some("video/mp4".into()),
			size: 128,
			media: model::EmbedMedia {
				width: 720,
				height: 1280,
				..Default::default()
			},
			spoiler: false,
			duration_ms: Some(11_817),
			waveform: Vec::new(),
		};
		let mut video = VideoUi::default();
		let show = |video: &mut VideoUi| {
			ctx.run_ui(
				egui::RawInput {
					screen_rect: Some(egui::Rect::from_min_size(
						egui::Pos2::ZERO,
						egui::vec2(436.0, 600.0),
					)),
					..Default::default()
				},
				|ui| {
					ui.set_width(420.0);
					video.show(
						ui,
						&message,
						&attachment,
						&mut crate::attachments::DownloadUi::default(),
						&mut None,
						false,
					);
				},
			)
			.drop_without_applying_deltas();
		};
		show(&mut video);
		// One queued poster request, still idle: playback never starts on its own,
		// and the playback command slot stays free for clicks.
		assert_eq!(video.poster_queue.len(), 1);
		assert_eq!(video.poster_queue[0], attachment);
		assert!(video.command.is_none() && video.active.is_none());
		// Draining (as the desktop does each frame) retries while unresolved;
		// the desktop pending set absorbs the duplicates.
		video.poster_queue.clear();
		show(&mut video);
		assert_eq!(video.poster_queue.len(), 1);
		// Bounds match the playback path; failures never cache a texture.
		assert!(!video.accept_poster(&ctx, Id(3), 0, 1, &[]));
		assert!(!video.accept_poster(&ctx, Id(3), 1921, 1, &[]));
		assert!(!video.accept_poster(&ctx, Id(3), 1, 1, &[0]));
		assert!(video.posters.is_empty());
		video.poster_failed(Id(9));
		assert!(video.posters.is_empty());
		// The decoded answer is cached and painted without further requests.
		assert!(video.accept_poster(&ctx, Id(3), 2, 1, &[9, 9, 9, 255, 1, 1, 1, 255]));
		assert_eq!(video.posters.len(), 1);
		assert_eq!(video.poster_bytes, 8);
		show(&mut video);
		assert!(video.command.is_none());
		assert_eq!(video.posters.len(), 1);
		// Budget eviction drops the oldest texture and its retry guard together.
		let big = vec![7; 1920 * 1080 * 4];
		assert!(video.accept_poster(&ctx, Id(4), 1920, 1080, &big));
		assert_eq!(video.posters.len(), 2);
		assert!(video.accept_poster(&ctx, Id(5), 1920, 1080, &big));
		assert_eq!(video.posters.len(), 1);
		assert!(!video.posters.contains_key(&Id(3)));
		assert!(!video.posters.contains_key(&Id(4)));
		assert!(!video.poster_seen.contains(&Id(3)));
		assert!(video.poster_bytes <= POSTER_BUDGET);
	}
}
