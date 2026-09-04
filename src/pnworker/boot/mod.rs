// Node boot profiles: starting a machine that is not running yet, because work is waiting for it.
//
// The pieces are deliberately separate. `profile` is the editable TOML file and its validation,
// `secrets` the credential values it refers to, `binding` which profile belongs to which node
// identity, `attempt` the durable record of one run, `exec` the HTTP sequence itself, and `manager`
// the only thing that decides a boot should happen at all.
//
// Two rules run through all of it. Unmet queued demand is the only trigger — an offline node is not
// one, and there is no command that boots a machine by hand. And a provider answering 200 is not a
// node: the scheduler uses a machine when it registers and is accepted, never before.
pub mod attempt;
pub mod binding;
pub mod exec;
pub mod manager;
pub mod profile;
pub mod secrets;
