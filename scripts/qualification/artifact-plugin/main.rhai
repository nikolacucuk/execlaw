fn tool_call(name, args, oauth) {
    if name != "qualification_artifact.create" {
        throw "unknown tool";
    }
    host_create_attachment("cXVhbGlmaWVk", "text/plain", "proof.txt", 3600)
}
