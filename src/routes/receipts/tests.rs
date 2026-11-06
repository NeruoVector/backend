use super::verify_landed;
use serde_json::json;
use uuid::Uuid;

#[test]
fn receipt_request_needs_only_the_confirmation_action() {
    let input: super::ReceiptInput = serde_json::from_value(json!({"confirmed":true})).unwrap();
    assert!(input.confirmed);
    assert!(serde_json::from_value::<super::ReceiptInput>(json!({"pickup_code":"1234"})).is_err());
}

#[test]
fn receipt_requires_current_disarmed_matching_destination_landing() {
    let id = Uuid::new_v4();
    let valid = json!({"timestamp":100.0,"order_id":id,"armed":false,
        "mission":{"order_id":id,"phase":"awaiting_receipt","arrived":true}});
    assert!(verify_landed(&serde_json::to_vec(&valid).unwrap(), id, 101.0).is_ok());
    for invalid in [
        json!({"armed":true}), json!({"timestamp":90.0}),
        json!({"timestamp":102.0}), json!({"order_id":Uuid::new_v4()}),
        json!({"mission":{"order_id":id,"phase":"landing","arrived":true}}),
        json!({"mission":{"order_id":id,"phase":"awaiting_receipt","arrived":false}}),
        json!({"mission":{"order_id":Uuid::new_v4(),"phase":"awaiting_receipt","arrived":true}}),
    ] {
        let mut state = valid.clone();
        for (key, value) in invalid.as_object().unwrap() { state[key] = value.clone(); }
        assert!(verify_landed(&serde_json::to_vec(&state).unwrap(), id, 101.0).is_err());
    }
}
