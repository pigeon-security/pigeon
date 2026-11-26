import Foundation

/// One setting of an instance capability, as the web's `CAPS[].fields` lists it.
struct CapabilityField: Hashable {
    let key: String
    let label: String
    /// A choice from these values (the record's site names) instead of free text.
    var options: [String]? = nil
    /// Comma-separated text sent as a list.
    var list = false
    var mono = false
    var isOptional = false
    var hint: String? = nil
}

/// An instance capability: the record field that enables it and what it adds to the console,
/// as `views/capabilities.js` `CAPS` describes it. Every change is one record diff through
/// `instance-put`.
struct CapabilitySpec: Identifiable {
    let cap: String
    let field: String
    let component: String
    let adds: [String]
    let what: String

    var id: String { cap }

    /// What the record must have first, or nil when it can be enabled.
    func needs(_ r: JSON) -> String? {
        switch cap {
        case "publishing": return r["sites"].items.isEmpty ? "a site in the record" : nil
        case "site_passkeys": return CapabilityCatalog.isOn(r, CapabilityCatalog.publishing) ? nil : "publishing"
        case "source": return RecordDiff.isNone(r["repo"]) ? "a repository in the record" : nil
        case "ci":
            return r["account"]["kind"].string == "home" && r["repo"]["kind"].string == "artifacts"
                ? nil : "a home-account Artifacts repository"
        case "observability": return RecordDiff.isNone(r["worker"]) ? "a Worker in the record" : nil
        default: return nil
        }
    }

    /// The settings the capability takes.
    func fields(_ r: JSON) -> [CapabilityField] {
        let sites = r["sites"].items.compactMap { $0["name"].string }
        switch cap {
        case "publishing":
            return [CapabilityField(key: "site", label: "Site", options: sites, hint: "The site that Publish and Roll back act on.")]
        case "site_passkeys":
            return [
                CapabilityField(key: "site", label: "Site", options: sites, hint: "Only a site on platform subdomains can be gated."),
                CapabilityField(key: "allow", label: "Who may sign in", list: true, hint: "Comma-separated user ids, or site for every visitor you invite."),
            ]
        case "storage":
            return [
                CapabilityField(key: "store", label: "Storage account", mono: true, hint: "A declared account, such as scaleway:archive."),
                CapabilityField(key: "bucket", label: "Bucket", mono: true),
                CapabilityField(key: "prefix", label: "Prefix", mono: true, isOptional: true, hint: "Ends with /. Empty for the whole bucket."),
            ]
        case "ci":
            return [CapabilityField(key: "jobs", label: "Job names", list: true, mono: true, hint: "Comma-separated, 1 to 64.")]
        case "ingest":
            return [
                CapabilityField(key: "source", label: "Source account", mono: true, hint: "A declared storage account, such as scaleway:field."),
                CapabilityField(key: "bucket", label: "Source bucket", mono: true),
                CapabilityField(key: "prefix", label: "Prefix", mono: true, isOptional: true),
                CapabilityField(key: "destination_bucket", label: "Destination bucket", mono: true),
            ]
        default:
            return []
        }
    }
}

/// The instance capabilities and the record edits that enable, configure and disable them.
enum CapabilityCatalog {
    static let publishing = CapabilitySpec(
        cap: "publishing", field: "publishing", component: "sites",
        adds: ["Site › Sites", "Site › Deployments"],
        what: "Publish one of the instance’s sites from its repository")
    static let sitePasskeys = CapabilitySpec(
        cap: "site_passkeys", field: "sites[].auth", component: "site_passkeys",
        adds: ["Site › Visitors"],
        what: "Gate a site’s platform subdomains behind visitor passkeys")

    static let all: [CapabilitySpec] = [
        publishing,
        sitePasskeys,
        CapabilitySpec(
            cap: "storage", field: "storage", component: "storage", adds: ["Storage › Buckets"],
            what: "Read the objects of one approved bucket"),
        CapabilitySpec(
            cap: "source", field: "source", component: "source", adds: ["Source › Repository"],
            what: "Read the record’s repository"),
        CapabilitySpec(
            cap: "ci", field: "ci", component: "ci", adds: ["CI › Jobs"],
            what: "See CI jobs of the record’s repository"),
        CapabilitySpec(
            cap: "observability", field: "observability", component: "observability",
            adds: ["Metrics › Workers"], what: "Read metrics of the instance’s Worker"),
        CapabilitySpec(
            cap: "ingest", field: "ingest", component: "ingest", adds: ["Ingest › Runs"],
            what: "Ingest from a declared source into a destination bucket"),
    ]

    /// The record's sites gated by visitor passkeys.
    static func passkeySites(_ r: JSON) -> [JSON] {
        r["sites"].items.filter { $0["auth"].string == "passkey" }
    }

    /// Whether `c` is enabled on record `r`.
    static func isOn(_ r: JSON, _ c: CapabilitySpec) -> Bool {
        if c.cap == "site_passkeys" { return !passkeySites(r).isEmpty && isOn(r, publishing) }
        return r[c.field]["state"].string == "enabled"
    }

    /// What disabling `c` also disables.
    static func dependents(_ r: JSON, _ c: CapabilitySpec) -> [CapabilitySpec] {
        c.cap == "publishing" && isOn(r, sitePasskeys) ? [sitePasskeys] : []
    }

    /// The capability's current settings, as text for the form.
    static func currentValues(_ r: JSON, _ c: CapabilitySpec) -> [String: String] {
        let now: JSON
        if c.cap == "site_passkeys" {
            if let s = passkeySites(r).first {
                now = ["site": s["name"], "allow": s["allow"].isNull ? [] : s["allow"]]
            } else {
                now = ["site": r["publishing"]["site"], "allow": ["site"]]
            }
        } else {
            now = r[c.field]
        }
        var out: [String: String] = [:]
        for f in c.fields(r) {
            var v = now[f.key]
            if v.isNull && f.key == "site" { v = r["publishing"]["site"] }
            if let a = v.array {
                out[f.key] = a.compactMap { $0.text }.joined(separator: ", ")
            } else {
                out[f.key] = v.text ?? ""
            }
        }
        return out
    }

    /// The form's text as the values the record takes (lists split on commas).
    static func values(_ text: [String: String], fields: [CapabilityField]) -> [String: JSON] {
        var out: [String: JSON] = [:]
        for f in fields {
            let raw = (text[f.key] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            let value: JSON = f.list ? JSON.array(ConsoleFormat.list(raw).map { JSON.string($0) }) : JSON.string(raw)
            out[f.key] = value
        }
        return out
    }

    /// The first required field left empty, if any.
    static func missing(_ values: [String: JSON], fields: [CapabilityField]) -> CapabilityField? {
        fields.first { f in
            guard !f.isOptional else { return false }
            let v = values[f.key] ?? .null
            return f.list ? v.items.isEmpty : (v.string ?? "").isEmpty
        }
    }

    /// Record `r` with `c` set to `values`.
    static func with(_ r: JSON, _ c: CapabilitySpec, values: [String: JSON]) -> JSON {
        if c.cap == "site_passkeys" {
            let site = values["site"]?.string
            let sites = r["sites"].items.map { s -> JSON in
                guard var o = s.object else { return s }
                o.removeValue(forKey: "auth")
                o.removeValue(forKey: "allow")
                if let site, o["name"]?.string == site {
                    o["auth"] = "passkey"
                    o["allow"] = values["allow"] ?? []
                }
                return .object(o)
            }
            return RecordDiff.setting(r, "sites", .array(sites))
        }
        var profile: [String: JSON] = ["state": "enabled"]
        for (k, v) in values where !RecordDiff.isNone(v) && v != "" {
            profile[k] = v
        }
        return RecordDiff.setting(r, c.field, .object(profile))
    }

    /// Record `r` without `c` and what needs it.
    static func without(_ r: JSON, _ c: CapabilitySpec) -> JSON {
        var next = r
        for d in [c] + dependents(r, c) {
            if d.cap == "site_passkeys" {
                let sites = next["sites"].items.map { s -> JSON in
                    guard var o = s.object else { return s }
                    o.removeValue(forKey: "auth")
                    o.removeValue(forKey: "allow")
                    return .object(o)
                }
                if !next["sites"].isNull { next = RecordDiff.setting(next, "sites", .array(sites)) }
            } else {
                next = RecordDiff.setting(next, d.field, nil)
            }
        }
        return next
    }
}
