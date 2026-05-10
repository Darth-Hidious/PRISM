// @generated automatically by Diesel CLI.

diesel::table! {
    conversations (conversation_id) {
        conversation_id -> Text,
        title -> Nullable<Text>,
        workspace_id -> BigInt,
        context -> Nullable<Text>,
        created_at -> Timestamp,
        updated_at -> Nullable<Timestamp>,
        metrics -> Nullable<Text>,
    }
}

diesel::table! {
    messages (id) {
        id -> Text,
        conversation_id -> Text,
        ordinal -> BigInt,
        role -> Text,
        content -> Nullable<Text>,
        tool_calls_json -> Nullable<Text>,
        tool_results_json -> Nullable<Text>,
        usage_json -> Nullable<Text>,
        created_at -> BigInt,
    }
}

diesel::table! {
    sync_outbox (id) {
        id -> Integer,
        conversation_id -> Text,
        low_ordinal -> BigInt,
        high_ordinal -> BigInt,
        attempts -> Integer,
        last_attempt_at -> Nullable<BigInt>,
        last_error -> Nullable<Text>,
        created_at -> BigInt,
    }
}
