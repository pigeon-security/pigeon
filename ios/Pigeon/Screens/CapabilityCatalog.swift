import Foundation

/// One setting of an instance capability, as the web's `CAPS[].fields` and `catalogueCaps`
/// list it.
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
    /// What each option is shown as, when not its value.
    var labels: [String: String]? = nil

    /// Whether the form's `values` give this field a value it accepts, as the web's `validate`
    /// checks DigitalOcean's settings. Every other field accepts what it is given.
    func accepts(_ values: [String: JSON]) -> Bool {
        let of = { (k: String) -> String in values[k]?.string ?? "" }
        let v = of(key)
        switch key {
        case "team_id":
            return Self.matches(v, "^[A-Za-z0-9_-]{1,128}$")
        case "spaces_region":
            return v.isEmpty ? of("spaces_bucket").isEmpty : Self.matches(v, "^[a-z]{2,8}[0-9]{1,2}$")
        case "spaces_bucket":
            return v.isEmpty
                ? of("spaces_region").isEmpty && of("spaces_prefix").isEmpty
                : Self.matches(v, "^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$")
        case "spaces_prefix":
            return v.utf8.count <= 1024
                && !v.unicodeScalars.contains { $0.value <= 0x1f || (0x7f...0x9f).contains($0.value) }
        default:
            return true
        }
    }

    private static func matches(_ text: String, _ pattern: String) -> Bool {
        text.range(of: pattern, options: .regularExpression) != nil
    }
}

/// A catalogue capability: enabled at its record field (`data.email`, `cloud.gateway`, …; else
/// `capabilities.<name>`), as `capabilities_versions` `catalogue` lists it (the web's
/// `catalogueCaps`; docs/plans/record-reshape.md).
struct CatalogueEntry {
    /// The capabilities it needs enabled beside it.
    let requires: [String]
    /// Where each of `requires` sits in the record.
    let requiresFields: [String]
    /// The record fields of the capabilities that sit inside this one (`data.email` inside
    /// `data`), relative to it: kept when it is configured, not shown as its settings.
    let children: [String]
    /// Whether the record is the home instance's.
    let home: Bool
    /// Whether the instance chooses where it runs: a linked instance, for an entry that offers
    /// a Worker of its own.
    let configurable: Bool
    /// The placements a linked instance may choose (`linked_placement_choices`).
    let choices: [String]
    /// The record fields of the catalogue capabilities that need this one, removed with it.
    let dependents: [String]
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
    /// What enabling it does, when that says more than what it adds.
    var effect: String? = nil
    /// Set for a catalogue capability.
    var catalogue: CatalogueEntry? = nil

    var id: String { cap }

    /// Whether this is Jobs on a linked instance, whose audit bucket is set up through
    /// `app_jobs_audit_plan`. The record accepts an audit bucket only there.
    var linkedJobs: Bool { cap == "jobs" && catalogue?.home == false }

    /// What the record must have first, or nil when it can be enabled.
    func needs(_ r: JSON) -> String? {
        if let entry = catalogue {
            if linkedJobs && (r["account"]["account_id"].string ?? "").isEmpty {
                return "a linked Cloudflare account"
            }
            let lack = zip(entry.requires, entry.requiresFields)
                .filter { RecordDiff.at(r, $0.1).isNull }
                .map { $0.0 }
            return lack.isEmpty ? nil : lack.joined(separator: ", ")
        }
        switch cap {
        case "publishing": return r["sites"].items.isEmpty ? "a site in the record" : nil
        case "site_passkeys": return CapabilityCatalog.isOn(r, CapabilityCatalog.publishing) ? nil : "publishing"
        case "source": return r["repo"]["kind"].isNull ? "a repository in the record" : nil
        case "data-read": return r["data"].isNull ? "data" : nil
        case "ci":
            return r["account"]["kind"].string == "home" && r["repo"]["kind"].string == "artifacts"
                ? nil : "a home-account Artifacts repository"
        case "observability": return RecordDiff.isNone(r["worker"]) ? "a Worker in the record" : nil
        default: return nil
        }
    }

    /// The settings the capability takes.
    func fields(_ r: JSON) -> [CapabilityField] {
        if let entry = catalogue { return catalogueFields(entry) }
        let sites = r["sites"].items.compactMap { $0["name"].string }
        switch cap {
        case "publishing":
            return [CapabilityField(key: "site", label: "Site", options: sites, hint: "The site that Publish and Roll back act on.")]
        case "site_passkeys":
            return [
                CapabilityField(key: "site", label: "Site", options: sites, hint: "Only a site on platform subdomains can be gated."),
                CapabilityField(key: "allow", label: "Who may sign in", list: true, hint: "Comma-separated user ids, or site for every visitor you invite."),
            ]
        case "data-read":
            return [
                CapabilityField(key: "store", label: "Storage account", mono: true, hint: "A declared account, such as scaleway:archive."),
                CapabilityField(key: "bucket", label: "Bucket", mono: true),
                CapabilityField(key: "prefix", label: "Prefix", mono: true, isOptional: true, hint: "Ends with /. Empty for the whole bucket."),
            ]
        case "ci":
            return [CapabilityField(key: "jobs", label: "Job names", list: true, mono: true, hint: "Comma-separated, 1 to 64.")]
        default:
            return []
        }
    }

    private func catalogueFields(_ entry: CatalogueEntry) -> [CapabilityField] {
        var out: [CapabilityField] = []
        if cap == "digitalocean" {
            out += [
                CapabilityField(
                    key: "team_id", label: "DigitalOcean team ID", mono: true,
                    hint: "Verification checks that the token belongs to this team."),
                CapabilityField(
                    key: "spaces_region", label: "Spaces region", mono: true, isOptional: true,
                    hint: "For example, nyc3. Leave region and bucket blank to keep Spaces off."),
                CapabilityField(key: "spaces_bucket", label: "Spaces bucket", mono: true, isOptional: true),
                CapabilityField(
                    key: "spaces_prefix", label: "Spaces prefix", mono: true, isOptional: true,
                    hint: "Leave blank to check the bucket root."),
                CapabilityField(
                    key: "droplet_health", label: "Droplet runner checks", options: ["no", "yes"],
                    hint: "Each runner check needs a separate passkey approval before compute starts.",
                    labels: ["no": "Off", "yes": "On"]),
            ]
        }
        if entry.configurable {
            // Only the placements there is a linked-account build for; the web lists the
            // others disabled.
            let own = entry.choices.contains("own_worker")
            out.append(
                CapabilityField(
                    key: "placement", label: "Worker placement",
                    options: ["inline", "own_worker"].filter { entry.choices.contains($0) },
                    hint: own ? nil : "Separate Worker requires a linked-account build for this capability.",
                    labels: ["inline": "Inline", "own_worker": "Separate Worker"]))
        }
        return out
    }
}

/// The instance capabilities and the record edits that enable, configure and disable them.
enum CapabilityCatalog {
    static let publishing = CapabilitySpec(
        cap: "publishing", field: "site.publishing", component: "sites",
        adds: ["Site › Sites", "Site › Deployments"],
        what: "Publish one of the instance’s sites from its repository")
    static let sitePasskeys = CapabilitySpec(
        cap: "site_passkeys", field: "site.passkeys", component: "site_passkeys",
        adds: ["Site › Visitors"],
        what: "Gate a site’s platform subdomains behind visitor passkeys")

    /// What each catalogue capability adds and does, as the web's `CATALOGUE_ADDS`.
    private static let catalogueAdds: [String: (adds: [String], what: String, effect: String?)] = [
        "architecture": (
            ["Platform › Architecture"],
            "Show this instance’s own setup: its Worker, bucket, hostnames and tools", nil
        ),
        "data-ingest-email": (
            [], "Enable read-only IMAP backfill",
            "The next Data plan authorizes signed historical imports with a dedicated key."
        ),
        "mcp-federation": ([], "Share approved Jobs reads through the platform MCP connection", nil),
        "jobs": (
            ["Jobs › Runs"], "Inspect this instance’s job history",
            "Shows instance-owned job history and exports terminal audit records to one private R2 bucket."
        ),
        "cloud": (
            [], "Enable Cloud Gateway status for this instance",
            "Adds cloud_status to the instance’s MCP endpoint."
        ),
        "digitalocean": (
            [], "Connect the instance to one DigitalOcean team",
            "Approves the expected team for this instance. Set and verify the token separately with a passkey."
        ),
        "secrets": (
            ["Access › Secrets"],
            "List the secrets in this instance’s own Cloudflare account (its Secrets Store)", nil
        ),
    ]

    /// The catalogue capabilities this instance may enable by name: the opt-in entries on home,
    /// the linked entries on a linked instance; never one every instance has, nor `redirects`.
    static func catalogue(_ versions: JSON, home: Bool) -> [CapabilitySpec] {
        let entries = versions["catalogue"].items
        return entries.compactMap { e -> CapabilitySpec? in
            guard let name = e["name"].string else { return nil }
            let offered = home ? e["opt_in"].bool == true : e["linked"].bool == true
            guard offered, e["always"].bool != true, name != "redirects" else { return nil }
            let placements = e["placements"].items.compactMap { $0.string }
            let choices = versions["linked_placement_choices"][name].array?.compactMap { $0.string }
                ?? ["inline"]
            let fieldOf = { (n: String) -> String in
                entries.first { $0["name"].string == n }?["field"].string ?? "capabilities.\(n)"
            }
            let field = fieldOf(name)
            let dependents = entries
                .filter { $0["requires"].items.contains(.string(name)) }
                .compactMap { $0["name"].string }
                .map(fieldOf)
            let children = entries.compactMap { $0["field"].string }
                .filter { $0.hasPrefix(field + ".") }
                .map { String($0.dropFirst(field.count + 1)) }
            let requires = e["requires"].items.compactMap { $0.string }
            let about = catalogueAdds[name]
            return CapabilitySpec(
                cap: name, field: field, component: name,
                adds: about?.adds ?? [], what: about?.what ?? "", effect: about?.effect,
                catalogue: CatalogueEntry(
                    requires: requires, requiresFields: requires.map(fieldOf), children: children,
                    home: home,
                    // Email runs where the data store runs: only `data` chooses.
                    configurable: !home && !["data-email", "data-ingest-email"].contains(name) && placements.contains("own_worker"),
                    choices: choices, dependents: dependents))
        }
    }

    static let all: [CapabilitySpec] = [
        publishing,
        sitePasskeys,
        CapabilitySpec(
            cap: "data-read", field: "data.read", component: "data", adds: ["Storage › Buckets"],
            what: "Read the objects of one approved bucket, beside the data store"),
        CapabilitySpec(
            cap: "source", field: "repo.source", component: "source", adds: ["Source › Repository"],
            what: "Read the record’s repository"),
        CapabilitySpec(
            cap: "ci", field: "ci", component: "ci", adds: ["CI › Jobs"],
            what: "See CI jobs of the record’s repository"),
        CapabilitySpec(
            cap: "observability", field: "observability", component: "observability",
            adds: ["Metrics › Workers"], what: "Read metrics of the instance’s Worker"),
    ]

    /// The record's sites gated by visitor passkeys: each `site.passkeys` names, as
    /// `{name, allow}`.
    static func passkeySites(_ r: JSON) -> [JSON] {
        (r["site"]["passkeys"].object ?? [:]).keys.sorted().map { name in
            let allow = r["site"]["passkeys"][name]["allow"]
            return ["name": .string(name), "allow": allow.isNull ? [] : allow]
        }
    }

    /// Whether `c` is enabled on record `r`.
    static func isOn(_ r: JSON, _ c: CapabilitySpec) -> Bool {
        if c.catalogue != nil { return !RecordDiff.at(r, c.field).isNull }
        if c.cap == "site_passkeys" { return !passkeySites(r).isEmpty && isOn(r, publishing) }
        return RecordDiff.at(r, c.field)["state"].string == "enabled"
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
                now = ["site": r["site"]["publishing"]["site"], "allow": ["site"]]
            }
        } else if let entry = c.catalogue {
            let settings = RecordDiff.at(r, c.field)
            var o: [String: JSON] = ["placement": "inline"]
            for (k, v) in settings.object ?? [:] where !entry.children.contains(k) && !(c.cap == "data" && k == "read") {
                o[k] = v
            }
            if c.cap == "digitalocean" {
                o["spaces_region"] = settings["spaces"]["region"]
                o["spaces_bucket"] = settings["spaces"]["bucket"]
                o["spaces_prefix"] = settings["spaces"]["prefix"]
                o["droplet_health"] = .string(settings["droplet_health"].bool == true ? "yes" : "no")
            }
            now = .object(o)
        } else {
            now = RecordDiff.at(r, c.field)
        }
        var out: [String: String] = [:]
        for f in c.fields(r) {
            var v = now[f.key]
            if v.isNull && f.key == "site" { v = r["site"]["publishing"]["site"] }
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

    /// The first field whose value it does not accept, if any.
    static func invalid(_ values: [String: JSON], fields: [CapabilityField]) -> CapabilityField? {
        fields.first { !$0.accepts(values) }
    }

    /// Record `r` with `c` set to `values`.
    static func with(_ r: JSON, _ c: CapabilitySpec, values: [String: JSON]) -> JSON {
        if let entry = c.catalogue {
            // The whole entry: the capabilities inside it (`data.email`) stay as they are.
            var settings = RecordDiff.at(r, c.field).object ?? [:]
            if c.linkedJobs { settings["audit_bucket"] = values["audit_bucket"] }
            if entry.configurable { settings["placement"] = values["placement"] }
            if c.cap == "digitalocean" {
                let text = { (k: String) -> String in values[k]?.string ?? "" }
                settings["team_id"] = values["team_id"]
                if text("droplet_health") == "yes" {
                    settings["droplet_health"] = .bool(true)
                } else {
                    settings.removeValue(forKey: "droplet_health")
                }
                let region = text("spaces_region"), bucket = text("spaces_bucket"), prefix = text("spaces_prefix")
                if !region.isEmpty || !bucket.isEmpty || !prefix.isEmpty {
                    var spaces: [String: JSON] = ["region": .string(region), "bucket": .string(bucket)]
                    if !prefix.isEmpty { spaces["prefix"] = .string(prefix) }
                    settings["spaces"] = .object(spaces)
                } else {
                    settings.removeValue(forKey: "spaces")
                }
            }
            return RecordDiff.setting(r, path: c.field, .object(settings))
        }
        if c.cap == "site_passkeys" {
            guard let site = values["site"]?.string, !site.isEmpty else {
                return RecordDiff.setting(r, path: c.field, nil)
            }
            let gated: JSON = .object([site: .object(["allow": values["allow"] ?? []])])
            return RecordDiff.setting(r, path: c.field, gated)
        }
        var profile: [String: JSON] = ["state": "enabled"]
        for (k, v) in values where !RecordDiff.isNone(v) && v != "" {
            profile[k] = v
        }
        return RecordDiff.setting(r, path: c.field, .object(profile))
    }

    /// Record `r` without `c` and what needs it.
    static func without(_ r: JSON, _ c: CapabilitySpec) -> JSON {
        if let entry = c.catalogue {
            var next = r
            for field in entry.dependents + [c.field] { next = RecordDiff.setting(next, path: field, nil) }
            return next
        }
        var next = r
        for d in [c] + dependents(r, c) {
            next = RecordDiff.setting(next, path: d.field, nil)
        }
        return next
    }
}
