//! An inert MCP sign-in handle. It identifies an attempt and its resulting
//! authorization generation; it is neither OAuth state nor a credential.
crate::id::prefixed_id! {
    /// Validated MCP sign-in attempt and authorization-generation identity.
    McpLoginId, "mcl"
}
