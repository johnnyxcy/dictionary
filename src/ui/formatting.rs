/// Compact counts use at most two decimal places, rounded with integer math.
pub(super) fn compact_count(value: Option<u128>) -> String {
    let Some(value) = value else {
        return "—".into();
    };
    if value < 1000 {
        return value.to_string();
    }

    let mut unit = value.ilog10() / 3;
    let step = 1000_u128.pow(unit) / 100;
    let mut hundredths = value / step + u128::from(value % step >= step / 2);
    // Carry rounding into the next unit instead of displaying 1000K or 1000M.
    if hundredths >= 100_000 {
        unit += 1;
        hundredths = 100;
    }
    let suffix = match unit {
        1 => "K".into(),
        2 => "M".into(),
        3 => "B".into(),
        4 => "T".into(),
        _ => format!("e{}", unit * 3),
    };
    let whole = hundredths / 100;
    match hundredths % 100 {
        0 => format!("{whole}{suffix}"),
        fraction if fraction % 10 == 0 => format!("{whole}.{}{suffix}", fraction / 10),
        fraction => format!("{whole}.{fraction:02}{suffix}"),
    }
}

/// Exact counts retain all integer precision and separate groups of three.
pub(super) fn exact_count(value: Option<u128>) -> String {
    let Some(value) = value else {
        return "—".into();
    };
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}
