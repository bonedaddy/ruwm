use core::cell::RefCell;
use core::fmt::Debug;

use embedded_graphics::prelude::{DrawTargetExt, Point, Size};
use embedded_graphics::primitives::Rectangle;
use serde::{Deserialize, Serialize};

use log::trace;

use enumset::{enum_set, EnumSet, EnumSetType};

use embassy_futures::select::select_array;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;

use embedded_svc::executor::asynch::Unblocker;

use channel_bridge::notification::Notification;

use crate::battery::{self, BatteryState};
use crate::keepalive::{self, RemainingTime};
use crate::screen::shapes::util::clear;
use crate::screen::shapes::Actions;
use crate::valve::{self, ValveState};
use crate::wm::{self, WaterMeterState};

pub use adaptors::*;
pub use shapes::Color;

use self::pages::{Battery, Summary};
use self::shapes::Action;

mod adaptors;
mod pages;
mod shapes;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Page {
    Summary = 0,
    Battery = 1,
}

impl Page {
    pub const fn new() -> Self {
        Self::Summary
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Summary => Self::Battery,
            Self::Battery => Self::Summary,
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Summary => Self::Battery,
            Self::Battery => Self::Summary,
        }
    }

    pub fn actions(&self) -> EnumSet<Action> {
        let actions = match self {
            Self::Summary => Action::OpenValve | Action::CloseValve | Action::Arm | Action::Disarm,
            Self::Battery => EnumSet::empty(),
        };

        let mut actions = actions.intersection(Action::active());

        if !actions.is_empty() {
            actions |= Action::Dismiss;
        }

        actions
    }
}

impl Default for Page {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, EnumSetType)]
pub enum DataSource {
    Page,
    Valve,
    WM,
    WMStats,
    Battery,
    RemainingTime,
}

#[derive(Default, Clone, Debug, Eq, PartialEq)]
pub struct ScreenState {
    changeset: EnumSet<DataSource>,
    active_page: Page,
    page_actions: Option<(EnumSet<Action>, Action)>,
}

impl ScreenState {
    pub const fn new() -> Self {
        Self {
            changeset: enum_set!(
                DataSource::Page
                    | DataSource::Valve
                    | DataSource::WM
                    | DataSource::WMStats
                    | DataSource::Battery
                    | DataSource::RemainingTime
            ),
            active_page: Page::new(),
            page_actions: None,
        }
    }

    pub fn valve(&self) -> Option<Option<ValveState>> {
        self.changed([DataSource::Valve, DataSource::Page])
            .then(|| valve::STATE.get())
    }

    pub fn wm(&self) -> Option<WaterMeterState> {
        self.changed([DataSource::WM, DataSource::Page])
            .then(|| wm::STATE.get())
    }

    pub fn battery(&self) -> Option<BatteryState> {
        self.changed([DataSource::Battery, DataSource::Page])
            .then(|| battery::STATE.get())
    }

    pub fn remaining_time(&self) -> Option<RemainingTime> {
        self.changed([DataSource::RemainingTime, DataSource::Page])
            .then(|| keepalive::STATE.get())
    }

    fn changed<const N: usize>(&self, changes: [DataSource; N]) -> bool {
        changes
            .iter()
            .find(|data_source| self.changeset.contains(**data_source))
            .is_some()
    }
}

pub(crate) static BUTTON1_PRESSED_NOTIF: Notification = Notification::new();
pub(crate) static BUTTON2_PRESSED_NOTIF: Notification = Notification::new();
pub(crate) static BUTTON3_PRESSED_NOTIF: Notification = Notification::new();
pub(crate) static VALVE_STATE_NOTIF: Notification = Notification::new();
pub(crate) static WM_STATE_NOTIF: Notification = Notification::new();
pub(crate) static WM_STATS_STATE_NOTIF: Notification = Notification::new();
pub(crate) static BATTERY_STATE_NOTIF: Notification = Notification::new();
pub(crate) static MQTT_STATE_NOTIF: Notification = Notification::new();
pub(crate) static WIFI_STATE_NOTIF: Notification = Notification::new();
pub(crate) static REMAINING_TIME_NOTIF: Notification = Notification::new();

static DRAW_REQUEST_NOTIF: Notification = Notification::new();

static STATE: Mutex<CriticalSectionRawMutex, RefCell<ScreenState>> =
    Mutex::new(RefCell::new(ScreenState::new()));

#[allow(clippy::too_many_arguments)]
pub async fn process() {
    loop {
        let (_future, index) = select_array([
            BUTTON1_PRESSED_NOTIF.wait(),
            BUTTON2_PRESSED_NOTIF.wait(),
            BUTTON3_PRESSED_NOTIF.wait(),
            VALVE_STATE_NOTIF.wait(),
            WM_STATE_NOTIF.wait(),
            BATTERY_STATE_NOTIF.wait(),
            REMAINING_TIME_NOTIF.wait(),
        ])
        .await;

        {
            STATE.lock(|screen_state| {
                let mut screen_state = screen_state.borrow_mut();

                match index {
                    0 => {
                        if let Some((actions, action)) = screen_state.page_actions {
                            screen_state.page_actions =
                                action.prev(&actions).map(|action| (actions, action));
                        } else {
                            screen_state.active_page = screen_state.active_page.prev();
                        }

                        screen_state.changeset.insert(DataSource::Page);
                    }
                    1 => {
                        if let Some((actions, action)) = screen_state.page_actions {
                            screen_state.page_actions =
                                action.next(&actions).map(|action| (actions, action));
                        } else {
                            screen_state.active_page = screen_state.active_page.next();
                        }

                        screen_state.changeset.insert(DataSource::Page);
                    }
                    2 => {
                        if let Some((_, action)) = screen_state.page_actions {
                            screen_state.page_actions = None;
                            action.trigger();
                        } else {
                            let actions = screen_state.active_page.actions();
                            screen_state.page_actions =
                                Action::first(&actions).map(|action| (actions, action));
                        }

                        screen_state.changeset.insert(DataSource::Page);
                    }
                    3 => {
                        screen_state.changeset.insert(DataSource::Valve);
                    }
                    4 => {
                        screen_state.changeset.insert(DataSource::WM);
                    }
                    5 => {
                        screen_state.changeset.insert(DataSource::Battery);
                    }
                    6 => {
                        screen_state.changeset.insert(DataSource::RemainingTime);
                    }
                    _ => unreachable!(),
                }
            });
        }

        DRAW_REQUEST_NOTIF.notify();
    }
}

pub async fn unblock_run_draw<U, D>(unblocker: U, mut display: D)
where
    U: Unblocker,
    D: Flushable<Color = Color> + Send + 'static,
    D::Error: Debug + Send + 'static,
{
    loop {
        DRAW_REQUEST_NOTIF.wait().await;

        let screen_state = STATE.lock(|screen_state| {
            let screen_state_prev = screen_state.borrow().clone();

            screen_state.borrow_mut().changeset = EnumSet::empty();

            screen_state_prev
        });

        display = unblocker
            .unblock(move || draw(display, screen_state))
            .await
            .unwrap();
    }
}

pub async fn run_draw<D>(mut display: D)
where
    D: Flushable<Color = Color>,
    D::Error: Debug,
{
    loop {
        DRAW_REQUEST_NOTIF.wait().await;

        let screen_state = STATE.lock(|screen_state| {
            let screen_state_prev = screen_state.borrow().clone();

            screen_state.borrow_mut().changeset = EnumSet::empty();

            screen_state_prev
        });

        display = draw(display, screen_state).unwrap();
    }
}

fn draw<D>(mut display: D, screen_state: ScreenState) -> Result<D, D::Error>
where
    D: Flushable<Color = Color>,
    D::Error: Debug,
{
    trace!("DRAWING: {:?}", screen_state);

    if screen_state.changeset.contains(DataSource::Page) {
        clear(&display.bounding_box(), &mut display)?;
    }

    match screen_state.active_page {
        Page::Summary => Summary::draw(
            &mut display,
            screen_state.valve().as_ref(),
            screen_state.wm().as_ref(),
            screen_state.battery().as_ref(),
        )?,
        Page::Battery => Battery::draw(&mut display, screen_state.battery().as_ref())?,
    }

    if let Some((actions, action)) = screen_state.page_actions {
        let bbox = display.bounding_box();

        let Size { width, .. } = bbox.size;

        let font = if width <= 128 {
            profont::PROFONT_12_POINT
        } else {
            profont::PROFONT_24_POINT
        };

        let bbox = display.bounding_box();

        let actions_shape = Actions {
            enabled: actions,
            selected: action,
            font,
            ..Default::default()
        };

        let actions_shape_size =
            Size::new(bbox.size.width - 10, actions_shape.preferred_size().height);

        let mut target = display.cropped(&Rectangle::new(
            Point::new(
                (bbox.size.width as i32 - actions_shape_size.width as i32) / 2,
                (bbox.size.height as i32 - actions_shape_size.height as i32) / 2,
            ),
            actions_shape_size,
        ));

        actions_shape.draw(&mut target)?;
    }

    display.flush()?;

    Ok(display)
}
