pub mod libpncurl;
pub mod libpnenv;
pub mod libpnmpeg;
pub mod libpnp2p;
pub mod pnworker;
pub mod libpnprotocol;
pub mod libpndb;
//
// -----------------------------
// Macro Builders
// -----------------------------
//

// ----- Schema Builder -----

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


// ----- Data Builder -----

#[macro_export]
macro_rules! pn_data {

    ([ $( $inner:tt ),* $(,)? ]) => {
        pandora_toolchain::libpnprotocol::core::TypeC::Multi(vec![
            $( pn_data!($inner) ),*
        ])
    };

    ($val:expr) => {
        pandora_toolchain::libpnprotocol::core::TypeC::Single(pandora_toolchain::libpnprotocol::core::Data { value: $val.to_string() })
    };
}


//
// ----- Full Info String Macro -----
// Builds schema + data and emits via Protocol
//

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


// ----- Data Builder -----

#[macro_export]
macro_rules! lib_pn_data {

    ([ $( $inner:tt ),* $(,)? ]) => {
        crate::libpnprotocol::core::TypeC::Multi(vec![
            $( lib_pn_data!($inner) ),*
        ])
    };

    ($val:expr) => {
        crate::libpnprotocol::core::TypeC::Single(crate::libpnprotocol::core::Data { value: $val.to_string() })
    };
}


//
// ----- Full Info String Macro -----
// Builds schema + data and emits via Protocol
//

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
