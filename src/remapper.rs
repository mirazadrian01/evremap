use crate::mapping::*;
use anyhow::*;
use evdevil::event::{EventKind, EventType, InputEvent, Key};
use evdevil::{Evdev, EventReader, uinput::UinputDevice};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::path::Path;
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug)]
enum KeyEventType {
    Release,
    Press,
    Repeat,
    Unknown(i32),
}

impl KeyEventType {
    fn from_value(value: i32) -> Self {
        match value {
            0 => KeyEventType::Release,
            1 => KeyEventType::Press,
            2 => KeyEventType::Repeat,
            _ => KeyEventType::Unknown(value),
        }
    }

    fn value(&self) -> i32 {
        match self {
            Self::Release => 0,
            Self::Press => 1,
            Self::Repeat => 2,
            Self::Unknown(n) => *n,
        }
    }
}

// fn timeval_diff(newer: &TimeVal, older: &TimeVal) -> Duration {
//     const MICROS_PER_SECOND: libc::time_t = 1000000;
//     let secs = newer.tv_sec - older.tv_sec;
//     let usecs = newer.tv_usec - older.tv_usec;
//
//     let (secs, usecs) = if usecs < 0 {
//         (secs - 1, usecs + MICROS_PER_SECOND)
//     } else {
//         (secs, usecs)
//     };
//
//     Duration::from_micros(((secs * MICROS_PER_SECOND) + usecs) as u64)
// }

pub struct InputMapper {
    input: EventReader,
    output: UinputDevice,
    /// If present in this map, the key is down since the instant
    /// of its associated value
    input_state: HashMap<Key, SystemTime>,

    mappings: Vec<Mapping>,

    /// The most recent candidate for a tap function is held here
    tapping: Option<Key>,

    output_keys: HashSet<Key>,

    /// Non-modifier input keys that are currently consumed by an active
    /// chord (Remap) mapping. When a chord partially breaks (e.g. the
    /// modifier is released while the non-modifier is still held) these
    /// keys are suppressed so they don't leak through as bare keypresses.
    /// Modifier keys are intentionally excluded so they remain available
    /// to participate in other chords (e.g. alt+q and alt+w can be
    /// triggered in sequence while alt is held).
    chord_keys: HashSet<Key>,

    modifier_keys: HashSet<Key>,
}

// fn enable_key_code(input: &mut Device, key: KeyCode) -> Result<()> {
//     input
//         .enable(EventCode::EV_KEY(key.clone()))
//         .context(format!("enable key {:?}", key))?;
//     Ok(())
// }

impl InputMapper {
    pub fn create_mapper<P: AsRef<Path>>(path: P, mappings: Vec<Mapping>) -> Result<Self> {
        let path = path.as_ref();
        let input = Evdev::open(path)
            .with_context(|| format!("failed to create new Device from file {}", path.display()))?;

        let modifier_keys: HashSet<Key> = mappings
            .iter()
            .filter_map(|m| match m {
                Mapping::ModifierKey { keys } => Some(keys.iter().cloned()),
                _ => None,
            })
            .flatten()
            .collect();

        let mut output_keys: HashSet<Key> = input.supported_keys()?.into_iter().collect();
        for m in &mappings {
            match m {
                Mapping::DualRole { hold, tap, ..} => {
                    output_keys.extend(hold);
                    output_keys.extend(tap);
                },
                Mapping::Remap { output, ..} => {
                    output_keys.extend(output);
                },
                Mapping::ModifierKey { keys } => {
                    output_keys.extend(keys);
                }
            }
        }

        let output = UinputDevice::builder()?
            .with_keys(output_keys)?
            .build(&format!("evremap Virtual input for {}", path.display()))
            .context(format!("creating UinputDevice from {}", path.display()))?;

        input
            .grab()
            .context(format!("grabbing exclusive access on {}", path.display()))?;
        let input = input.into_reader()
            .context(format!("turning into reader {}", path.display()))?;

        Ok(Self {
            input,
            output,
            input_state: HashMap::new(),
            output_keys: HashSet::new(),
            tapping: None,
            mappings,
            chord_keys: HashSet::new(),
            modifier_keys: modifier_keys,
        })
    }

    pub fn run_mapper(&mut self) -> Result<Infallible> {
        log::info!("Going into read loop");

        loop {
            let event = {
                let mut events = self.input.events();
                let Some(event) = events.next() else {
                    continue;
                };
                event?
            };

            if let EventKind::Key(key_event) = event.kind() {
                log::trace!("IN {:?}", event);
                self.update_with_event(&event, key_event.key())?
            } else {
                log::trace!("PASSTHRU {:?}", event);
                self.output.write_events(&[event])?
            }
        }
        // loop {
        //     let (status, event) = self
        //         .input
        //         .next_event(ReadFlag::NORMAL | ReadFlag::BLOCKING)?;
        //     match status {
        //         evdev_rs::ReadStatus::Success => {
        //             if let EventCode::EV_KEY(ref key) = event.event_code {
        //                 log::trace!("IN {:?}", event);
        //                 self.update_with_event(&event, key.clone())?;
        //             } else {
        //                 log::trace!("PASSTHRU {:?}", event);
        //                 self.output.write_event(&event)?;
        //             }
        //         }
        //         evdev_rs::ReadStatus::Sync => bail!("ReadStatus::Sync!"),
        //     }
        // }
    }

    /// Compute the effective set of keys that are pressed
    fn compute_keys(&self) -> HashSet<Key> {
        // Remove keys pressed before modifier
        let oldest_modifier_pressed_time = self.input_state
            .iter()
            .filter(|(k, _)| is_modifier(k, &self.modifier_keys))
            .min_by_key(|(_, t)| *t)
            .map(|(_, t)| *t);

        let mut keys: HashSet<Key> = self.input_state
            .iter()
            .filter(|(k, t)| {
                is_modifier(k, &self.modifier_keys) ||
                    oldest_modifier_pressed_time
                    .as_ref()
                    .map(|cutoff| *t > cutoff)
                    .unwrap_or(true)
            })
        .map(|(k, _)| (*k).clone())
        .collect();



        // First phase is to apply any DualRole mappings as they are likely to
        // be used to produce modifiers when held.
        for map in &self.mappings {
            if let Mapping::DualRole { input, hold, .. } = map {
                if keys.contains(input) {
                    keys.remove(input);
                    for h in hold {
                        keys.insert(h.clone());
                    }
                }
            }
        }

        let mut keys_minus_remapped = keys.clone();

        // Second pass to apply Remap items
        for map in &self.mappings {
            if let Mapping::Remap { input, output } = map {
                if input.is_subset(&keys_minus_remapped) {
                    for i in input {
                        keys.remove(i);
                        if !is_modifier(i, &self.modifier_keys) {
                            keys_minus_remapped.remove(i);
                        }
                    }
                    for o in output {
                        keys.insert(o.clone());
                        // Outputs that apply are not visible as
                        // inputs for later remap rules
                        if !is_modifier(o, &self.modifier_keys) {
                            keys_minus_remapped.remove(o);
                        }
                    }
                } else {
                    // Chord is broken (e.g. modifier released while non-modifier
                    // still held, or vice-versa). Suppress any non-modifier keys
                    // that were part of this chord so they don't leak through.
                    //
                    // Modifier keys are intentionally left alone: a shared
                    // modifier like alt must remain available so that other
                    // chords (alt+w, alt+q, …) continue to work while alt
                    // is held.
                    for i in input {
                        if !is_modifier(i, &self.modifier_keys) && self.chord_keys.contains(i) {
                            keys.remove(i);
                            keys_minus_remapped.remove(i);
                        }
                    }
                }
            }
        }

        keys
    }

    /// Compute the difference between our desired set of keys
    /// and the set of keys that are currently pressed in the
    /// output device.
    /// Release any keys that should not be pressed, and then
    /// press any keys that should be pressed.
    ///
    /// When releasing, release modifiers last so that mappings
    /// that produce eg: CTRL-C don't emit a random C character
    /// when released.
    ///
    /// Similarly, when pressing, emit modifiers first so that
    /// we don't emit C and then CTRL for such a mapping.
    fn compute_and_apply_keys(&mut self, time: &SystemTime) -> Result<()> {
        let desired_keys = self.compute_keys();
        let mut to_release: Vec<Key> = self
            .output_keys
            .difference(&desired_keys)
            .cloned()
            .collect();

        let mut to_press: Vec<Key> = desired_keys
            .difference(&self.output_keys)
            .cloned()
            .collect();

        if !to_release.is_empty() {
            to_release.sort_by(|a, b| modifiers_last(a, b, &self.modifier_keys));
            self.emit_keys(&to_release, time, KeyEventType::Release)?;
        }
        if !to_press.is_empty() {
            to_press.sort_by(|a, b| modifiers_first(a, b, &self.modifier_keys));
            self.emit_keys(&to_press, time, KeyEventType::Press)?;
        }
        Ok(())
    }

    fn lookup_dual_role_mapping(&self, code: Key) -> Option<Mapping> {
        for map in &self.mappings {
            if let Mapping::DualRole { input, .. } = map {
                if *input == code {
                    // A DualRole mapping has the highest precedence
                    // so we've found our match
                    return Some(map.clone());
                }
            }
        }
        None
    }

    fn lookup_mapping(&self, code: Key) -> Option<Mapping> {
        let mut candidates = vec![];

        for map in &self.mappings {
            match map {
                Mapping::DualRole { input, .. } => {
                    if *input == code {
                        // A DualRole mapping has the highest precedence
                        // so we've found our match
                        return Some(map.clone());
                    }
                }
                Mapping::Remap { input, .. } => {
                    // Look for a mapping that includes the current key.
                    // If part of a chord, all of its component keys must
                    // also be pressed.
                    let mut code_matched = false;
                    let mut all_matched = true;
                    for i in input {
                        if *i == code {
                            code_matched = true;
                        } else if !self.input_state.contains_key(i) {
                            all_matched = false;
                            break;
                        }
                    }
                    if code_matched && all_matched {
                        candidates.push(map);
                    }
                }
                Mapping::ModifierKey { .. } => ()
            }
        }

        // Any matches must be Remap entries.  We want the one
        // with the most active keys
        candidates.sort_by(|a, b| match (a, b) {
            (Mapping::Remap { input: input_a, .. }, Mapping::Remap { input: input_b, .. }) => {
                input_a.len().cmp(&input_b.len()).reverse()
            }
            _ => unreachable!(),
        });

        candidates.get(0).map(|&m| m.clone())
    }

    pub fn update_with_event(&mut self, event: &InputEvent, code: Key) -> Result<()> {
        let event_type = KeyEventType::from_value(event.raw_value());
        match event_type {
            KeyEventType::Release => {
                let pressed_at = match self.input_state.remove(&code) {
                    None => {
                        self.write_event(event)?;
                        return Ok(());
                    }
                    Some(p) => p,
                };

                // Remove from chord tracking on release.
                // Only non-modifiers are ever added to chord_keys, but the
                // guard here is harmless and makes the invariant explicit.
                if !is_modifier(&code, &self.modifier_keys) {
                    self.chord_keys.remove(&code);
                }

                self.compute_and_apply_keys(&event.time())?;

                if let Some(Mapping::DualRole { tap, .. }) =
                    self.lookup_dual_role_mapping(code.clone())
                {
                    // If released quickly enough, becomes a tap press.
                    if let Some(tapping) = self.tapping.take() {
                        if tapping == code
                            && event.time()
                                .duration_since(pressed_at)
                                .map_or(false, |d| d < Duration::from_millis(200))
                        {
                            self.emit_keys(&tap, &event.time(), KeyEventType::Press)?;
                            self.emit_keys(&tap, &event.time(), KeyEventType::Release)?;
                        }
                    }
                }
            }
            KeyEventType::Press => {
                self.input_state.insert(code.clone(), event.time().clone());

                match self.lookup_mapping(code.clone()) {
                    Some(Mapping::Remap { ref input, .. }) => {
                        // Register non-modifier input keys as active chord members.
                        // Modifiers are excluded so they stay available for other
                        // chords while held (alt+q then alt+w etc.).
                        for i in input {
                            if !is_modifier(i, &self.modifier_keys) {
                                self.chord_keys.insert(i.clone());
                            }
                        }
                        self.compute_and_apply_keys(&event.time())?;
                        self.tapping.replace(code);
                    }
                    Some(_) => {
                        self.compute_and_apply_keys(&event.time())?;
                        self.tapping.replace(code);
                    }
                    None => {
                        // Just pass it through
                        self.cancel_pending_tap();
                        self.compute_and_apply_keys(&event.time())?;
                    }
                }
            }
            KeyEventType::Repeat => {
                match self.lookup_mapping(code.clone()) {
                    Some(Mapping::DualRole { hold, .. }) => {
                        self.emit_keys(&hold, &event.time(), KeyEventType::Repeat)?;
                    }
                    Some(Mapping::Remap { input, .. }) => {
                        // Check whether the full chord is still satisfied.
                        let input_set: HashSet<Key> = input.iter().cloned().collect();
                        let currently_held: HashSet<Key> =
                            self.input_state.keys().cloned().collect();
                        if input_set.is_subset(&currently_held) {
                            // Full chord held — suppress repeat entirely.
                            // We don't want qqqqq (raw input leaking) or
                            // 111111 (mapped output repeating); the user
                            // asked for silence while the chord is held.
                        } else {
                            // Chord not fully satisfied — pass the raw key
                            // repeat through unchanged.
                            self.write_event(event)?;
                        }
                    }
                    Some(_) => (),
                    None => {
                        // Just pass it through
                        self.cancel_pending_tap();
                        self.write_event(event)?;
                    }
                }
            }
            KeyEventType::Unknown(_) => {
                self.write_event(event)?;
            }
        }

        Ok(())
    }

    fn cancel_pending_tap(&mut self) {
        self.tapping.take();
    }

    fn emit_keys(
        &mut self,
        key: &[Key],
        time: &SystemTime,
        event_type: KeyEventType,
    ) -> Result<()> {
        for k in key {
            let event = make_event(k.clone(), time, event_type);
            self.write_event(&event)?;
        }
        Ok(())
    }

    // fn write_event_and_sync(&mut self, event: &InputEvent) -> Result<()> {
    //     self.write_event(event)?;
    //     self.generate_sync_event(&event.time())?;
    //     Ok(())
    // }

    fn write_event(&mut self, event: &InputEvent) -> Result<()> {
        log::trace!("OUT: {:?}", event);
        self.output.write_events(&[*event])?;

        if let EventKind::Key(key_event) = event.kind() {
            match KeyEventType::from_value(event.raw_value()) {
                KeyEventType::Press | KeyEventType::Repeat => {
                    self.output_keys.insert(key_event.key());
                }
                KeyEventType::Release => {
                    self.output_keys.remove(&key_event.key());
                }
                KeyEventType::Unknown(_) => {}
            }
        }
        Ok(())
    }
    // fn generate_sync_event(&self, time: &SystemTime) -> Result<()> {
    //     self.output.write_event(&InputEvent::new(
    //         time,
    //         &EventCode::EV_SYN(evdev_rs::enums::EV_SYN::SYN_REPORT),
    //         0,
    //     ))?;
    //     Ok(())
    // }
}

fn make_event(key: Key, time: &SystemTime, event_type: KeyEventType) -> InputEvent {
    InputEvent::new(EventType::KEY, key.raw(), event_type.value())
        .with_time(*time)
}

fn is_modifier(key: &Key, modifier_keys: &HashSet<Key>) -> bool {
    modifier_keys.contains(key)
    // match key {
    //     Key::KEY_FN
    //     | Key::KEY_LEFTALT
    //     | Key::KEY_RIGHTALT
    //     | Key::KEY_LEFTMETA
    //     | Key::KEY_RIGHTMETA
    //     | Key::KEY_LEFTCTRL
    //     | Key::KEY_RIGHTCTRL
    //     | Key::KEY_LEFTSHIFT
    //     | Key::KEY_RIGHTSHIFT => true,
    //     _ => false,
    // }
}

/// Orders modifier keys ahead of non-modifier keys.
fn modifiers_first(a: &Key, b: &Key, modifier_keys: &HashSet<Key>) -> Ordering {
    if is_modifier(a, modifier_keys) {
        if is_modifier(b, modifier_keys) {
            Ordering::Equal
        } else {
            Ordering::Less
        }
    } else if is_modifier(b, modifier_keys) {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

fn modifiers_last(a: &Key, b: &Key, modifier_keys: &HashSet<Key>) -> Ordering {
    modifiers_first(a, b, modifier_keys).reverse()
}
