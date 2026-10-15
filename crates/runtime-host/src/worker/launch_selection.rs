//! Keep the appinfo choice made by the client through the native runtime handoff.
pub(super) fn select(
    requested: Option<u32>,
    available: &[i32],
    compatibility: bool,
) -> Result<u32, String> {
    // Windows launches use the separately validated compatibility plan; the
    // macOS runtime need not advertise those Windows-only option IDs.
    if compatibility {
        return Ok(requested.unwrap_or(0));
    }
    if available.iter().any(|value| *value < 0) {
        return Err("Runtime returned an invalid launch option ID".into());
    }
    if let Some(requested) = requested {
        return available
            .iter()
            .any(|value| *value as u32 == requested)
            .then_some(requested)
            .ok_or_else(|| {
                "Выбранный вариант запуска изменился. Обновите библиотеку и повторите запуск".into()
            });
    }
    match available {
        [only] => Ok(*only as u32),
        [] => Err("No launch options available".into()),
        _ => Err(
            "В сохранённых данных нет ID варианта запуска. Обновите библиотеку перед запуском"
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_selected_sparse_option_instead_of_first_runtime_option() {
        assert_eq!(select(Some(19), &[2, 19], false).unwrap(), 19);
    }

    #[test]
    fn mismatched_or_ambiguous_metadata_never_launches_another_option() {
        assert!(select(Some(19), &[2, 7], false).is_err());
        assert!(select(None, &[2, 7], false).is_err());
        assert!(select(None, &[-1], false).is_err());
    }

    #[test]
    fn legacy_single_option_and_windows_compatibility_remain_supported() {
        assert_eq!(select(None, &[7], false).unwrap(), 7);
        assert_eq!(select(Some(19), &[], true).unwrap(), 19);
        assert!(select(None, &[], false).is_err());
    }
}
