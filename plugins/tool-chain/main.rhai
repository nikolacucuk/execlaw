// tool-chain scaffold runtime.
//
// v0.1.0 is a manifest-first skeleton. The host registers the tool
// descriptors now; executable behaviour is wired in a later phase.

fn tool_call(tool_name, args) {
    if tool_name == "chain.plan" {
        return #{ ok: false, error: "not_implemented", message: "chain.plan scaffold only in v0.1.0" };
    }
    if tool_name == "chain.execute" {
        return #{ ok: false, error: "not_implemented", message: "chain.execute scaffold only in v0.1.0" };
    }
    return #{ ok: false, error: "unknown_tool", tool: tool_name };
}
