use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Min–max scaled sparkline, one bar per value.
pub fn sparkline(values: &[Decimal]) -> String {
    let (Some(min), Some(max)) = (values.iter().min(), values.iter().max()) else {
        return String::new();
    };
    let range = (*max - *min).to_f64().unwrap_or(0.0);
    values
        .iter()
        .map(|v| {
            if range == 0.0 {
                return BARS[3];
            }
            let t = (*v - *min).to_f64().unwrap_or(0.0) / range;
            BARS[(t * (BARS.len() - 1) as f64).round() as usize]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_to_range() {
        let v: Vec<Decimal> = [1, 2, 3, 4, 5, 6, 7, 8].into_iter().map(Decimal::from).collect();
        assert_eq!(sparkline(&v), "▁▂▃▄▅▆▇█");
        assert_eq!(sparkline(&[Decimal::ONE, Decimal::ONE]), "▄▄");
        assert_eq!(sparkline(&[]), "");
    }
}
