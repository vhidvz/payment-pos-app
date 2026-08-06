//! DE39 response-code -> message.
//!
//! DIALECT-SPECIFIC table (not standard ISO-8583), transcribed from
//! `TSSP1126Commander.GetResponseMessageText` via the reference library.
//! This component defines its own private code space that happens to reuse
//! some standard numbers — trust this table, not general ISO-8583 references.

pub fn response_message(code: &str) -> Option<&'static str> {
    Some(match code {
        "00" => "Transaction completed successfully",
        "01" => "Card was retracted",
        "02" => "Transaction amount cannot be less than the minimum amount",
        "03" => "No connection with the device",
        "04" => "Invalid information",
        "05" => "Zero-Rial balance due",
        "06" => "Error receiving information",
        "07" => "No permission for this operation",
        "08" => "Transaction not found",
        "09" => "Invalid terminal",
        "10" => "Error in response",
        "12" => "Invalid transaction",
        "14" => "Initialization error",
        // protocol states, not decline reasons — included for convenience
        "15" => "Request accepted, transaction in progress",
        "17" => "Transaction finished",
        "20" => "Invalid response",
        "26" => "Transaction error",
        "27" => "This bill has already been paid",
        "28" => "Not payable",
        "30" => "Data format error",
        "33" => "Card's expiration date has passed",
        "34" => "Security warning",
        "38" => "Number of incorrect PIN entries exceeds the allowed limit",
        "43" => "Security warning",
        "51" => "Insufficient balance",
        "55" => "Invalid card PIN",
        "57" => "This transaction is not permitted for the cardholder",
        "58" => "This transaction is not permitted for this terminal",
        "61" => "Transaction amount exceeds the allowed limit",
        "63" => "Security warning",
        "68" => "Response not received in time",
        "69" => "Number of incorrect PIN entries exceeds the allowed limit",
        "71" => "Invalid terminal",
        "75" => "Number of incorrect PIN entries exceeds the allowed limit",
        "78" => "Card is inactive",
        "79" => "Invalid amount",
        "80" => "No response from the card issuer",
        "84" => "No response from the card issuer",
        "91" => "No response from the card issuer",
        "92" => "Amounts do not match",
        "96" => "Unknown error",
        "97" => "No connection with the host/center",
        "98" => "Operation cancelled by user",
        "99" => "Card reader did not receive a response in time",
        _ => return None,
    })
}
