#[path = "lib/mod.rs"]
pub mod lib;
pub mod pnworker;
pub mod libkagami;

#[macro_export]
macro_rules! pn_schema {

    ([ $( $inner:tt ),* $(,)? ]) => {
        Schema::Multi(vec![
            $( pn_schema!($inner) ),*
        ])
    };

    (leaf) => {
        Schema::Leaf
    };
}

#[macro_export]
macro_rules! pn_data {

    ([ $( $inner:tt ),* $(,)? ]) => {
        pandora_toolchain::lib::protocol::core::TypeC::Multi(vec![
            $( pn_data!($inner) ),*
        ])
    };

    ($val:expr) => {
        pandora_toolchain::lib::protocol::core::TypeC::Single(pandora_toolchain::lib::protocol::core::Data { value: $val.to_string() })
    };
}

#[macro_export]
macro_rules! pn_emit {

    (
        protocol = $protocol:expr,
        negkey = $neg:expr,
        schema = $schema:tt,
        data = $data:tt
    ) => {{
        let __schema = pn_schema!($schema);
        let __data = pn_data!($data);
        $protocol.build_info_string($neg, &__schema, &__data)
    }};
}

#[macro_export]
macro_rules! lib_pn_schema {

    ([ $( $inner:tt ),* $(,)? ]) => {
        Schema::Multi(vec![
            $( lib_pn_schema!($inner) ),*
        ])
    };

    (leaf) => {
        Schema::Leaf
    };
}

#[macro_export]
macro_rules! lib_pn_data {

    ([ $( $inner:tt ),* $(,)? ]) => {
        crate::lib::protocol::core::TypeC::Multi(vec![
            $( lib_pn_data!($inner) ),*
        ])
    };

    ($val:expr) => {
        crate::lib::protocol::core::TypeC::Single(crate::lib::protocol::core::Data { value: $val.to_string() })
    };
}

#[macro_export]
macro_rules! lib_pn_emit {

    (
        protocol = $protocol:expr,
        negkey = $neg:expr,
        schema = $schema:tt,
        data = $data:tt
    ) => {{
        let __schema = lib_pn_schema!($schema);
        let __data = lib_pn_data!($data);
        $protocol.build_info_string($neg, &__schema, &__data)
    }};
}
