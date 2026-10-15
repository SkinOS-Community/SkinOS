//! Wine-side filtering for a positively identified mapped SDL gamepad.
//! This environment belongs to Wine commands, never the Valve SDL host.
use crate::input_devices::Device;
use std::collections::BTreeMap;

pub fn for_empty() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("NOTPROTON_RAW_CONTROLLERS".into(), "1".into()),
        ("SDL_GAMECONTROLLER_IGNORE_DEVICES".into(), String::new()),
        (
            "SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT".into(),
            String::new(),
        ),
        ("SKINOS_INPUT_FILTER_EXACT".into(), "1".into()),
    ])
}

pub fn for_device(device: &Device, live: &[Device]) -> Result<BTreeMap<String, String>, String> {
    if !device.is_gamepad
        || device.sdl_id == 0
        || live.iter().filter(|candidate| *candidate == device).count() != 1
        || live
            .iter()
            .filter(|candidate| candidate.sdl_id == device.sdl_id)
            .count()
            != 1
    {
        return Err(
            "Controller filtering requires one matching current SDL gamepad identity".into(),
        );
    }
    if device.is_virtual {
        // SDL virtual devices live only in this process; Wine cannot discover
        // them as host HID. Do not hide a real pad sharing their emulated VID.
        return Ok(for_empty());
    }
    if device.vendor_id == 0 || device.product_id == 0 {
        return Err(
            "Контроллер не сообщил VID/PID: безопасно отключить дублирующий ввод пока невозможно"
                .into(),
        );
    }
    if live.iter().any(|candidate| {
        candidate.sdl_id != device.sdl_id
            && !candidate.is_virtual
            && candidate.vendor_id == device.vendor_id
            && candidate.product_id == device.product_id
    }) {
        return Err("Несколько устройств имеют одинаковый VID/PID; избирательный фильтр Steam Input недоступен".into());
    }
    let mut result = for_empty();
    result.insert("NOTPROTON_RAW_CONTROLLERS".into(), "0".into());
    result.insert(
        "SDL_GAMECONTROLLER_IGNORE_DEVICES".into(),
        format!("0x{:04x}/0x{:04x}", device.vendor_id, device.product_id),
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pad(id: u32) -> Device {
        Device {
            sdl_id: id,
            name: "fixture".into(),
            vendor_id: 0x054c,
            product_id: 0x09cc,
            gamepad_type: 4,
            is_gamepad: true,
            is_virtual: false,
        }
    }
    #[test]
    fn physical_filter_is_exact_and_wine_side_only() {
        let device = pad(1);
        let policy = for_device(&device, std::slice::from_ref(&device)).unwrap();
        assert_eq!(policy["SDL_GAMECONTROLLER_IGNORE_DEVICES"], "0x054c/0x09cc");
        assert_eq!(policy["NOTPROTON_RAW_CONTROLLERS"], "0");
        assert_eq!(policy["SKINOS_INPUT_FILTER_EXACT"], "1");
        assert!(!policy.values().any(|value| value.contains("0x045e/0x028e")));
    }
    #[test]
    fn unknown_or_shared_model_cannot_hide_an_unmapped_device() {
        let device = pad(1);
        let mut other = pad(2);
        other.is_gamepad = false;
        assert!(for_device(&device, &[device.clone(), other]).is_err());
        let mut unknown = pad(1);
        unknown.vendor_id = 0;
        assert!(for_device(&unknown, std::slice::from_ref(&unknown)).is_err());
        assert!(for_device(&device, &[]).is_err());
    }
    #[test]
    fn virtual_fixture_never_hides_its_emulated_physical_model() {
        let mut virtual_pad = pad(1);
        virtual_pad.is_virtual = true;
        let physical = pad(2);
        assert_eq!(
            for_device(&virtual_pad, &[virtual_pad.clone(), physical]).unwrap(),
            for_empty()
        );
    }
}
