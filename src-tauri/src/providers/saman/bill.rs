//! Iranian bill identifier helpers — a faithful port of `Common.CalcCheckDigitBillId`,
//! `ValidatePaymentId`, `GetBillAmount` and `GetBillType` from the reference SDK.
//!
//! The pair is self-validating (two mod-11 check digits) and the amount is
//! encoded inside the payment id — decode it locally before paying.

use serde::Serialize;

/// Weighted mod-11 check digit, returned as a character code (`digit + '0'`).
fn calc_check_digit(input: &str) -> u8 {
    let mut weight: u32 = 2;
    let mut sum: u32 = 0;
    for ch in input.bytes().rev() {
        sum += (ch as u32 - 48) * weight;
        weight += 1;
        if weight > 7 {
            weight = 2;
        }
    }
    sum %= 11;
    sum = if sum != 1 && sum != 0 { 11 - sum } else { 0 };
    (sum + 48) as u8
}

fn strip_leading_zeros(s: &str) -> &str {
    s.trim_start_matches('0')
}

/// Amount encoded in a payment id: `int(payId[0 .. len-5]) * 1000` Rials.
pub fn bill_amount_rials(payment_id: &str) -> u64 {
    let p = strip_leading_zeros(payment_id);
    if p.len() <= 5 {
        return 0;
    }
    let head = &p[..p.len() - 5];
    head.parse::<u64>().map(|n| n * 1000).unwrap_or(0)
}

#[derive(Debug, Clone, Serialize)]
pub struct BillCategoryInfo {
    pub code: i32,
    pub en: String,
    pub fa: String,
}

/// Bill category from the second-to-last digit of the bill id.
pub fn bill_category(bill_id: &str) -> BillCategoryInfo {
    let b = strip_leading_zeros(bill_id);
    let bytes = b.as_bytes();
    let code = if bytes.len() >= 2 {
        bytes[bytes.len() - 2] as i32 - 48
    } else {
        -1
    };
    let (en, fa) = match code {
        1 => ("Water", "قبض آب"),
        2 => ("Electricity", "قبض برق"),
        3 => ("Gas", "قبض گاز"),
        4 => ("Telephone", "قبض تلفن"),
        5 => ("Mobile", "قبض موبایل"),
        6 => ("Municipality (old)", "قبض شهرداری قدیم"),
        7 => ("Municipality", "قبض شهرداری"),
        9 => ("Traffic fines", "جرایم راهنمایی و رانندگی"),
        _ => ("unknown", ""),
    };
    BillCategoryInfo { code, en: en.into(), fa: fa.into() }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillValidation {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bill_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_rials: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_en: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_fa: Option<String>,
}

fn fail(reason: &str) -> BillValidation {
    BillValidation {
        ok: false,
        reason: Some(reason.to_string()),
        bill_id: None,
        payment_id: None,
        amount_rials: None,
        category: None,
        category_en: None,
        category_fa: None,
    }
}

/// Validate a bill id / payment id pair and decode what it means.
///
/// Both must be 6–13 digits after stripping leading zeros; the payment id's
/// second-to-last digit must check over the payment id body, and its last digit
/// must check over `billId + paymentId body`.
pub fn validate_bill(bill_id_raw: &str, payment_id_raw: &str) -> BillValidation {
    let bill_id = strip_leading_zeros(bill_id_raw.trim());
    let payment_id = strip_leading_zeros(payment_id_raw.trim());

    if bill_id.is_empty()
        || payment_id.is_empty()
        || !bill_id.bytes().all(|b| b.is_ascii_digit())
        || !payment_id.bytes().all(|b| b.is_ascii_digit())
    {
        return fail("both ids must contain digits only");
    }
    if bill_id.len() < 6 || bill_id.len() > 13 {
        return fail("billId must be 6-13 digits (after stripping leading zeros)");
    }
    if payment_id.len() < 6 || payment_id.len() > 13 {
        return fail("paymentId must be 6-13 digits (after stripping leading zeros)");
    }

    let self_ok = calc_check_digit(&payment_id[..payment_id.len() - 2])
        == payment_id.as_bytes()[payment_id.len() - 2];
    if !self_ok {
        return fail("paymentId's own check digit is wrong");
    }
    let cross_input = format!("{}{}", bill_id, &payment_id[..payment_id.len() - 1]);
    let cross_ok = calc_check_digit(&cross_input) == payment_id.as_bytes()[payment_id.len() - 1];
    if !cross_ok {
        return fail("cross check digit failed — this paymentId is not for this billId");
    }

    let cat = bill_category(bill_id);
    BillValidation {
        ok: true,
        reason: None,
        bill_id: Some(bill_id.to_string()),
        payment_id: Some(payment_id.to_string()),
        amount_rials: Some(bill_amount_rials(payment_id)),
        category: Some(cat.code),
        category_en: Some(cat.en),
        category_fa: Some(cat.fa),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a self-consistent (billId, paymentId) pair the same way the issuer does.
    fn make_pair(bill_body: &str, pay_body: &str) -> (String, String) {
        let d1 = calc_check_digit(bill_body) as char;
        let bill_id = format!("{bill_body}{d1}");
        let p2 = calc_check_digit(pay_body) as char;
        let with_self = format!("{pay_body}{p2}");
        let p3 = calc_check_digit(&format!("{bill_id}{with_self}")) as char;
        (bill_id, format!("{with_self}{p3}"))
    }

    #[test]
    fn valid_pair_roundtrips() {
        // body ends in '2' so after the check digit is appended, the second-to-last
        // digit of the full bill id is 2 (electricity)
        let (bill, pay) = make_pair("123452", "8790123");
        let v = validate_bill(&bill, &pay);
        assert!(v.ok, "expected ok, got {:?}", v.reason);
        assert_eq!(v.category, Some(2));
        assert_eq!(v.amount_rials, Some(bill_amount_rials(&pay)));
    }

    #[test]
    fn tampered_payment_id_fails() {
        let (bill, pay) = make_pair("123452", "8790123");
        let mut bad = pay.into_bytes();
        bad[0] = if bad[0] == b'9' { b'8' } else { bad[0] + 1 };
        let v = validate_bill(&bill, std::str::from_utf8(&bad).unwrap());
        assert!(!v.ok);
    }

    #[test]
    fn amount_decoding() {
        assert_eq!(bill_amount_rials("00012345678"), 123_000); // head "123" * 1000
        assert_eq!(bill_amount_rials("12345"), 0);
    }

    #[test]
    fn non_digits_rejected() {
        assert!(!validate_bill("12a4567", "1234567").ok);
        assert!(!validate_bill("", "").ok);
    }
}
