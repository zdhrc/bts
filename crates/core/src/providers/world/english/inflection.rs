//! English verb forms from a packaged lexicon or explicit spelling choices.

use std::sync::OnceLock;

use super::{Error, Verb};

impl Verb {
    /// Looks up a reviewed English verb by its lowercase base form.
    ///
    /// The packaged vocabulary uses forms such as `gotten`, `canceled`, and
    /// `traveled`. Supply explicit forms when another regional or lexical form
    /// is intended. Unknown words return `None` rather than guessed inflections.
    pub fn lookup(base: &str) -> Option<Self> {
        known_verbs().iter().find(|verb| verb.base == base).cloned()
    }

    /// Constructs a regular English verb using suffix spelling rules.
    ///
    /// The caller asserts that the verb is regular. Reviewed stress and spelling
    /// exceptions are built in. Unknown endings that need a stress decision
    /// return an error. Use [`Self::regular_with_doubling`] or [`Self::from_forms`]
    /// to supply that decision. Known irregular verbs require [`Self::lookup`]
    /// or explicit forms.
    pub fn regular(base: &str) -> Result<Self, Error> {
        validate_form(base, "verb base")?;
        reject_modal(base)?;
        if IRREGULAR.iter().any(|forms| forms[0] == base) {
            return Err(Error::InvalidField {
                field: "verb base",
                reason: "known irregular verb requires lookup or explicit forms",
            });
        }
        let double = if DOUBLE_FINAL.contains(&base) {
            true
        } else if KEEP_FINAL.contains(&base) || !may_double(base) {
            false
        } else {
            return Err(Error::InvalidField {
                field: "verb base",
                reason: "final consonant spelling needs an explicit doubling choice",
            });
        };
        regular_forms(base, double)
    }

    /// Constructs a regular verb with an explicit final-consonant doubling choice.
    ///
    /// This controls `stop` to `stopped` and `stopping`, for example. The caller
    /// supplies the lexical stress decision. It does not override irregular
    /// past forms, which must be supplied through [`Self::from_forms`].
    pub fn regular_with_doubling(base: &str, double_final: bool) -> Result<Self, Error> {
        validate_form(base, "verb base")?;
        reject_modal(base)?;
        if IRREGULAR.iter().any(|forms| forms[0] == base) {
            return Err(Error::InvalidField {
                field: "verb base",
                reason: "known irregular verb requires lookup or explicit forms",
            });
        }
        if double_final && !may_double(base) {
            return Err(Error::InvalidField {
                field: "verb base",
                reason: "doubling requires a final consonant after a single vowel",
            });
        }
        regular_forms(base, double_final)
    }

    /// Supplies all lexical forms, including irregular and regional variants.
    ///
    /// Forms must be lowercase ASCII words. Their grammatical correctness is
    /// caller-owned. Particles and objects belong in the predicate complement.
    pub fn from_forms(
        base: &str,
        third_person_singular: &str,
        past: &str,
        past_participle: &str,
        present_participle: &str,
    ) -> Result<Self, Error> {
        for (field, form) in [
            ("verb base", base),
            ("third person singular verb", third_person_singular),
            ("past verb", past),
            ("past participle", past_participle),
            ("present participle", present_participle),
        ] {
            validate_form(form, field)?;
        }
        Ok(Self {
            base: base.into(),
            third_person_singular: third_person_singular.into(),
            past: past.into(),
            past_participle: past_participle.into(),
            present_participle: present_participle.into(),
        })
    }
}

pub(crate) fn known_verbs() -> &'static [Verb] {
    static VERBS: OnceLock<Vec<Verb>> = OnceLock::new();
    VERBS.get_or_init(|| {
        IRREGULAR
            .iter()
            .map(|forms| Verb::from_forms(forms[0], forms[1], forms[2], forms[3], forms[4]).expect("packaged verb forms"))
            .chain(REGULAR.iter().map(|base| Verb::regular(base).expect("packaged regular verb")))
            .collect()
    })
}

fn validate_form(form: &str, field: &'static str) -> Result<(), Error> {
    if form.is_empty() || !form.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return Err(Error::InvalidField {
            field,
            reason: "expected a lowercase English word containing only ASCII letters",
        });
    }
    Ok(())
}

fn reject_modal(base: &str) -> Result<(), Error> {
    if matches!(
        base,
        "can" | "could" | "may" | "might" | "must" | "shall" | "should" | "will" | "would"
    ) {
        return Err(Error::InvalidField {
            field: "verb base",
            reason: "modal auxiliaries require the clause modal field or explicit lexical forms",
        });
    }
    Ok(())
}

fn regular_forms(base: &str, double_final: bool) -> Result<Verb, Error> {
    let consonant_y = base.ends_with('y') && base.len() > 1 && !is_vowel(base.as_bytes()[base.len() - 2]);
    let third = if consonant_y {
        format!("{}ies", &base[..base.len() - 1])
    } else if base.ends_with(['s', 'x', 'z']) || base.ends_with("ch") || base.ends_with("sh") || base.ends_with('o') {
        if base.ends_with('z') && double_final {
            format!("{base}zes")
        } else if base == "stomach" {
            format!("{base}s")
        } else if matches!(base, "echo" | "veto" | "embargo") || !base.ends_with('o') {
            format!("{base}es")
        } else {
            format!("{base}s")
        }
    } else {
        format!("{base}s")
    };
    let stem = if double_final {
        format!("{base}{}", char::from(base.as_bytes()[base.len() - 1]))
    } else if matches!(base, "panic" | "picnic" | "mimic" | "traffic" | "frolic") {
        format!("{base}k")
    } else {
        base.to_owned()
    };
    let past = if consonant_y {
        format!("{}ied", &base[..base.len() - 1])
    } else if base.ends_with('e') {
        format!("{base}d")
    } else {
        format!("{stem}ed")
    };
    let present_participle = if let Some(stem) = base.strip_suffix("ie") {
        format!("{stem}ying")
    } else if base.ends_with('e')
        && !base.ends_with("ee")
        && !base.ends_with("oe")
        && !base.ends_with("ye")
        && !matches!(base, "singe" | "tinge" | "segue")
    {
        format!("{}ing", &base[..base.len() - 1])
    } else {
        format!("{stem}ing")
    };
    Verb::from_forms(base, &third, &past, &past, &present_participle)
}

fn is_vowel(byte: u8) -> bool {
    matches!(byte, b'a' | b'e' | b'i' | b'o' | b'u')
}

fn may_double(base: &str) -> bool {
    let bytes = base.as_bytes();
    bytes.len() >= 3
        && (!is_vowel(bytes[bytes.len() - 3]) || (bytes.len() >= 4 && &bytes[bytes.len() - 4..bytes.len() - 2] == b"qu"))
        && is_vowel(bytes[bytes.len() - 2])
        && matches!(
            bytes[bytes.len() - 1],
            b'b' | b'd' | b'g' | b'l' | b'm' | b'n' | b'p' | b'r' | b's' | b't' | b'z'
        )
}

const DOUBLE_FINAL: &[&str] = &[
    "admit", "ban", "beg", "chat", "clap", "commit", "compel", "control", "defer", "drag", "drop", "equip", "fit", "flag",
    "format", "gas", "grab", "hop", "hug", "jog", "log", "nod", "occur", "omit", "pat", "permit", "plan", "plug", "prefer",
    "prop", "quiz", "refer", "regret", "rob", "rub", "scan", "ship", "shop", "skip", "slam", "slip", "snap", "spot", "step",
    "stop", "submit", "tap", "tip", "transfer", "trap", "trim", "trip", "wrap",
];

const KEEP_FINAL: &[&str] = &[
    "alert", "answer", "benefit", "bus", "cancel", "consider", "cover", "credit", "debit", "deliver", "deposit", "develop",
    "discover", "edit", "enter", "filter", "focus", "gather", "happen", "limit", "listen", "monitor", "number", "offer",
    "open", "order", "profit", "question", "recover", "register", "remember", "render", "reopen", "suffer", "target", "travel",
    "visit", "wait", "water", "wonder",
];

const REGULAR: &[&str] = &[
    "accept",
    "access",
    "acknowledge",
    "activate",
    "add",
    "adjust",
    "admit",
    "advise",
    "affect",
    "agree",
    "alert",
    "allow",
    "analyze",
    "answer",
    "apologize",
    "appear",
    "apply",
    "appreciate",
    "approve",
    "arrange",
    "arrive",
    "ask",
    "assess",
    "assign",
    "assist",
    "assume",
    "attach",
    "attempt",
    "attend",
    "authorize",
    "avoid",
    "back",
    "believe",
    "belong",
    "bill",
    "block",
    "book",
    "borrow",
    "bounce",
    "browse",
    "calculate",
    "call",
    "cancel",
    "care",
    "carry",
    "cause",
    "change",
    "charge",
    "chat",
    "check",
    "claim",
    "clarify",
    "clean",
    "clear",
    "click",
    "close",
    "collect",
    "combine",
    "compare",
    "complain",
    "complete",
    "configure",
    "confirm",
    "connect",
    "consider",
    "contact",
    "contain",
    "continue",
    "contribute",
    "control",
    "convert",
    "copy",
    "correct",
    "count",
    "cover",
    "crack",
    "crash",
    "create",
    "credit",
    "cross",
    "damage",
    "deactivate",
    "debit",
    "decide",
    "decline",
    "decrease",
    "deduct",
    "define",
    "delay",
    "delete",
    "deliver",
    "demand",
    "deny",
    "depend",
    "deploy",
    "deposit",
    "describe",
    "deserve",
    "design",
    "detect",
    "determine",
    "develop",
    "die",
    "disable",
    "disagree",
    "disconnect",
    "discover",
    "discuss",
    "display",
    "dispute",
    "double",
    "doubt",
    "download",
    "drop",
    "dry",
    "earn",
    "edit",
    "email",
    "enable",
    "end",
    "enjoy",
    "enter",
    "escalate",
    "escape",
    "estimate",
    "evaluate",
    "exceed",
    "exchange",
    "exclude",
    "exist",
    "expand",
    "expect",
    "experience",
    "expire",
    "explain",
    "explore",
    "export",
    "express",
    "extend",
    "fail",
    "file",
    "fill",
    "finish",
    "fix",
    "flag",
    "follow",
    "force",
    "format",
    "forward",
    "gather",
    "generate",
    "guarantee",
    "guess",
    "handle",
    "happen",
    "help",
    "hire",
    "hope",
    "identify",
    "ignore",
    "impact",
    "import",
    "improve",
    "include",
    "increase",
    "indicate",
    "inform",
    "inspect",
    "install",
    "insure",
    "intend",
    "investigate",
    "invite",
    "involve",
    "issue",
    "join",
    "jump",
    "kick",
    "last",
    "launch",
    "learn",
    "lift",
    "like",
    "limit",
    "link",
    "list",
    "listen",
    "live",
    "load",
    "locate",
    "lock",
    "log",
    "look",
    "love",
    "mail",
    "manage",
    "mark",
    "match",
    "measure",
    "mention",
    "merge",
    "message",
    "mind",
    "miss",
    "modify",
    "monitor",
    "move",
    "name",
    "need",
    "negotiate",
    "note",
    "notice",
    "notify",
    "number",
    "obtain",
    "occur",
    "offer",
    "open",
    "order",
    "organize",
    "own",
    "park",
    "pass",
    "pause",
    "perform",
    "permit",
    "phone",
    "photograph",
    "pick",
    "place",
    "plan",
    "play",
    "point",
    "post",
    "prefer",
    "prepare",
    "present",
    "prevent",
    "price",
    "print",
    "proceed",
    "process",
    "promise",
    "promote",
    "protect",
    "provide",
    "pull",
    "purchase",
    "push",
    "qualify",
    "query",
    "question",
    "quote",
    "raise",
    "rate",
    "reach",
    "realize",
    "receive",
    "recognize",
    "recommend",
    "record",
    "recover",
    "reduce",
    "refer",
    "refund",
    "register",
    "reject",
    "relate",
    "release",
    "rely",
    "remain",
    "remember",
    "remind",
    "remove",
    "render",
    "renew",
    "rent",
    "reopen",
    "repair",
    "repeat",
    "replace",
    "reply",
    "report",
    "request",
    "require",
    "reschedule",
    "reserve",
    "resolve",
    "respond",
    "restart",
    "restore",
    "result",
    "retry",
    "return",
    "review",
    "rush",
    "sample",
    "save",
    "scan",
    "schedule",
    "score",
    "search",
    "seem",
    "select",
    "separate",
    "serve",
    "settle",
    "share",
    "ship",
    "shop",
    "sign",
    "simplify",
    "skip",
    "smash",
    "smell",
    "smile",
    "solve",
    "sort",
    "sound",
    "start",
    "stay",
    "stop",
    "store",
    "stress",
    "struggle",
    "submit",
    "succeed",
    "suggest",
    "suit",
    "supply",
    "support",
    "surprise",
    "suspend",
    "switch",
    "sync",
    "talk",
    "test",
    "thank",
    "tip",
    "touch",
    "tow",
    "track",
    "train",
    "transfer",
    "translate",
    "transport",
    "travel",
    "treat",
    "trust",
    "try",
    "turn",
    "type",
    "unlock",
    "update",
    "upgrade",
    "upload",
    "use",
    "validate",
    "verify",
    "view",
    "visit",
    "wait",
    "waive",
    "walk",
    "want",
    "warn",
    "wash",
    "watch",
    "weigh",
    "welcome",
    "wish",
    "work",
    "worry",
];

// Each row supplies base, third person, past, past participle, and present participle.
const IRREGULAR: &[[&str; 5]] = &[
    ["arise", "arises", "arose", "arisen", "arising"],
    ["awake", "awakes", "awoke", "awoken", "awaking"],
    ["be", "is", "was", "been", "being"],
    ["bear", "bears", "bore", "borne", "bearing"],
    ["beat", "beats", "beat", "beaten", "beating"],
    ["become", "becomes", "became", "become", "becoming"],
    ["begin", "begins", "began", "begun", "beginning"],
    ["bend", "bends", "bent", "bent", "bending"],
    ["bet", "bets", "bet", "bet", "betting"],
    ["bind", "binds", "bound", "bound", "binding"],
    ["bite", "bites", "bit", "bitten", "biting"],
    ["bleed", "bleeds", "bled", "bled", "bleeding"],
    ["blow", "blows", "blew", "blown", "blowing"],
    ["break", "breaks", "broke", "broken", "breaking"],
    ["bring", "brings", "brought", "brought", "bringing"],
    ["build", "builds", "built", "built", "building"],
    ["burst", "bursts", "burst", "burst", "bursting"],
    ["buy", "buys", "bought", "bought", "buying"],
    ["catch", "catches", "caught", "caught", "catching"],
    ["choose", "chooses", "chose", "chosen", "choosing"],
    ["come", "comes", "came", "come", "coming"],
    ["cost", "costs", "cost", "cost", "costing"],
    ["cut", "cuts", "cut", "cut", "cutting"],
    ["deal", "deals", "dealt", "dealt", "dealing"],
    ["dig", "digs", "dug", "dug", "digging"],
    ["do", "does", "did", "done", "doing"],
    ["draw", "draws", "drew", "drawn", "drawing"],
    ["drink", "drinks", "drank", "drunk", "drinking"],
    ["drive", "drives", "drove", "driven", "driving"],
    ["eat", "eats", "ate", "eaten", "eating"],
    ["fall", "falls", "fell", "fallen", "falling"],
    ["feed", "feeds", "fed", "fed", "feeding"],
    ["feel", "feels", "felt", "felt", "feeling"],
    ["fight", "fights", "fought", "fought", "fighting"],
    ["find", "finds", "found", "found", "finding"],
    ["flee", "flees", "fled", "fled", "fleeing"],
    ["fly", "flies", "flew", "flown", "flying"],
    ["forbid", "forbids", "forbade", "forbidden", "forbidding"],
    ["forget", "forgets", "forgot", "forgotten", "forgetting"],
    ["forgive", "forgives", "forgave", "forgiven", "forgiving"],
    ["freeze", "freezes", "froze", "frozen", "freezing"],
    ["get", "gets", "got", "gotten", "getting"],
    ["give", "gives", "gave", "given", "giving"],
    ["go", "goes", "went", "gone", "going"],
    ["grow", "grows", "grew", "grown", "growing"],
    ["hang", "hangs", "hung", "hung", "hanging"],
    ["have", "has", "had", "had", "having"],
    ["hear", "hears", "heard", "heard", "hearing"],
    ["hide", "hides", "hid", "hidden", "hiding"],
    ["hit", "hits", "hit", "hit", "hitting"],
    ["hold", "holds", "held", "held", "holding"],
    ["hurt", "hurts", "hurt", "hurt", "hurting"],
    ["keep", "keeps", "kept", "kept", "keeping"],
    ["know", "knows", "knew", "known", "knowing"],
    ["lay", "lays", "laid", "laid", "laying"],
    ["lead", "leads", "led", "led", "leading"],
    ["leave", "leaves", "left", "left", "leaving"],
    ["lend", "lends", "lent", "lent", "lending"],
    ["let", "lets", "let", "let", "letting"],
    ["lose", "loses", "lost", "lost", "losing"],
    ["make", "makes", "made", "made", "making"],
    ["mean", "means", "meant", "meant", "meaning"],
    ["meet", "meets", "met", "met", "meeting"],
    ["overpay", "overpays", "overpaid", "overpaid", "overpaying"],
    ["pay", "pays", "paid", "paid", "paying"],
    ["prove", "proves", "proved", "proven", "proving"],
    ["put", "puts", "put", "put", "putting"],
    ["quit", "quits", "quit", "quit", "quitting"],
    ["read", "reads", "read", "read", "reading"],
    ["rebuild", "rebuilds", "rebuilt", "rebuilt", "rebuilding"],
    ["resend", "resends", "resent", "resent", "resending"],
    ["reset", "resets", "reset", "reset", "resetting"],
    ["ride", "rides", "rode", "ridden", "riding"],
    ["ring", "rings", "rang", "rung", "ringing"],
    ["rise", "rises", "rose", "risen", "rising"],
    ["run", "runs", "ran", "run", "running"],
    ["say", "says", "said", "said", "saying"],
    ["see", "sees", "saw", "seen", "seeing"],
    ["seek", "seeks", "sought", "sought", "seeking"],
    ["sell", "sells", "sold", "sold", "selling"],
    ["send", "sends", "sent", "sent", "sending"],
    ["set", "sets", "set", "set", "setting"],
    ["shake", "shakes", "shook", "shaken", "shaking"],
    ["shoot", "shoots", "shot", "shot", "shooting"],
    ["show", "shows", "showed", "shown", "showing"],
    ["shrink", "shrinks", "shrank", "shrunk", "shrinking"],
    ["shut", "shuts", "shut", "shut", "shutting"],
    ["sing", "sings", "sang", "sung", "singing"],
    ["sink", "sinks", "sank", "sunk", "sinking"],
    ["sit", "sits", "sat", "sat", "sitting"],
    ["sleep", "sleeps", "slept", "slept", "sleeping"],
    ["slide", "slides", "slid", "slid", "sliding"],
    ["speak", "speaks", "spoke", "spoken", "speaking"],
    ["spend", "spends", "spent", "spent", "spending"],
    ["spin", "spins", "spun", "spun", "spinning"],
    ["split", "splits", "split", "split", "splitting"],
    ["spread", "spreads", "spread", "spread", "spreading"],
    ["stand", "stands", "stood", "stood", "standing"],
    ["steal", "steals", "stole", "stolen", "stealing"],
    ["stick", "sticks", "stuck", "stuck", "sticking"],
    ["strike", "strikes", "struck", "struck", "striking"],
    ["swear", "swears", "swore", "sworn", "swearing"],
    ["sweep", "sweeps", "swept", "swept", "sweeping"],
    ["swim", "swims", "swam", "swum", "swimming"],
    ["swing", "swings", "swung", "swung", "swinging"],
    ["take", "takes", "took", "taken", "taking"],
    ["teach", "teaches", "taught", "taught", "teaching"],
    ["tear", "tears", "tore", "torn", "tearing"],
    ["tell", "tells", "told", "told", "telling"],
    ["think", "thinks", "thought", "thought", "thinking"],
    ["throw", "throws", "threw", "thrown", "throwing"],
    ["understand", "understands", "understood", "understood", "understanding"],
    ["undo", "undoes", "undid", "undone", "undoing"],
    ["wake", "wakes", "woke", "woken", "waking"],
    ["wear", "wears", "wore", "worn", "wearing"],
    ["win", "wins", "won", "won", "winning"],
    ["wind", "winds", "wound", "wound", "winding"],
    ["withdraw", "withdraws", "withdrew", "withdrawn", "withdrawing"],
    ["write", "writes", "wrote", "written", "writing"],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_packaged_verb_has_its_forms() {
        // a regular verb that cannot tell whether to double its last letter would fail here
        let verbs = known_verbs();
        let mut bases: Vec<_> = verbs.iter().map(|verb| verb.base.as_str()).collect();
        bases.sort_unstable();
        let before = bases.len();
        bases.dedup();
        assert_eq!(bases.len(), before, "a verb is packaged twice");
        let forms = |base: &str| {
            let verb = Verb::lookup(base).unwrap_or_else(|| panic!("{base} is not packaged"));
            [
                verb.third_person_singular.clone(),
                verb.past.clone(),
                verb.present_participle.clone(),
            ]
        };
        assert_eq!(forms("file"), ["files", "filed", "filing"]);
        assert_eq!(forms("email"), ["emails", "emailed", "emailing"]);
        assert_eq!(forms("flag"), ["flags", "flagged", "flagging"]);
        assert_eq!(forms("reopen"), ["reopens", "reopened", "reopening"]);
        assert_eq!(forms("qualify"), ["qualifies", "qualified", "qualifying"]);
        assert_eq!(forms("burst"), ["bursts", "burst", "bursting"]);
        assert_eq!(forms("reset"), ["resets", "reset", "resetting"]);
    }

    #[test]
    fn regular_spelling_changes_are_lexical_and_predictable() {
        for (base, third, past, ing) in [
            ("walk", "walks", "walked", "walking"),
            ("try", "tries", "tried", "trying"),
            ("play", "plays", "played", "playing"),
            ("watch", "watches", "watched", "watching"),
            ("fix", "fixes", "fixed", "fixing"),
            ("agree", "agrees", "agreed", "agreeing"),
            ("die", "dies", "died", "dying"),
            ("use", "uses", "used", "using"),
            ("dye", "dyes", "dyed", "dyeing"),
            ("singe", "singes", "singed", "singeing"),
            ("segue", "segues", "segued", "segueing"),
            ("stomach", "stomachs", "stomached", "stomaching"),
            ("embargo", "embargoes", "embargoed", "embargoing"),
            ("stop", "stops", "stopped", "stopping"),
            ("prefer", "prefers", "preferred", "preferring"),
            ("open", "opens", "opened", "opening"),
            ("visit", "visits", "visited", "visiting"),
            ("panic", "panics", "panicked", "panicking"),
            ("format", "formats", "formatted", "formatting"),
            ("discover", "discovers", "discovered", "discovering"),
            ("equip", "equips", "equipped", "equipping"),
            ("quiz", "quizzes", "quizzed", "quizzing"),
            ("gas", "gases", "gassed", "gassing"),
            ("focus", "focuses", "focused", "focusing"),
            ("cancel", "cancels", "canceled", "canceling"),
        ] {
            let verb = Verb::regular(base).unwrap();
            assert_eq!(verb.third_person_singular, third, "{base}");
            assert_eq!(verb.past, past, "{base}");
            assert_eq!(verb.past_participle, past, "{base}");
            assert_eq!(verb.present_participle, ing, "{base}");
        }
    }

    #[test]
    fn unknown_stress_requires_an_explicit_spelling_decision() {
        assert!(Verb::regular("allot").is_err());
        let verb = Verb::regular_with_doubling("allot", true).unwrap();
        assert_eq!(verb.past, "allotted");
        assert_eq!(verb.present_participle, "allotting");
        assert_eq!(Verb::regular_with_doubling("pivot", false).unwrap().past, "pivoted");
        assert!(Verb::regular_with_doubling("play", true).is_err());
        assert_eq!(Verb::regular_with_doubling("equip", true).unwrap().past, "equipped");
        assert!(Verb::regular("go").is_err());
        assert!(Verb::regular("must").is_err());
        assert!(Verb::regular_with_doubling("should", false).is_err());
    }

    #[test]
    fn irregular_forms_and_explicit_variants_are_preserved() {
        for (base, past, participle, ing) in [
            ("be", "was", "been", "being"),
            ("go", "went", "gone", "going"),
            ("write", "wrote", "written", "writing"),
            ("run", "ran", "run", "running"),
            ("read", "read", "read", "reading"),
            ("get", "got", "gotten", "getting"),
            ("have", "had", "had", "having"),
        ] {
            let verb = Verb::lookup(base).unwrap();
            assert_eq!(
                (
                    verb.past.as_str(),
                    verb.past_participle.as_str(),
                    verb.present_participle.as_str()
                ),
                (past, participle, ing)
            );
        }
        let british = Verb::from_forms("travel", "travels", "travelled", "travelled", "travelling").unwrap();
        assert_eq!(british.past, "travelled");
        assert_eq!(british.present_participle, "travelling");
        assert!(Verb::lookup("unknownverb").is_none());
    }

    #[test]
    fn packaged_vocabulary_has_valid_unique_entries() {
        let verbs = known_verbs();
        let mut bases = std::collections::HashSet::new();
        for verb in verbs {
            assert!(bases.insert(&verb.base), "duplicate {}", verb.base);
            assert_eq!(Verb::lookup(&verb.base).as_ref(), Some(verb));
            assert!(
                Verb::from_forms(
                    &verb.base,
                    &verb.third_person_singular,
                    &verb.past,
                    &verb.past_participle,
                    &verb.present_participle
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn lexical_forms_reject_fragments_and_identifiers() {
        for base in ["", "Check", "check in", "check_in", "check-42", "café", " go"] {
            assert!(Verb::regular(base).is_err(), "{base}");
            assert!(Verb::lookup(base).is_none(), "{base}");
        }
        assert!(Verb::from_forms("check", "checks", "checked", "checked", "checking in").is_err());
    }
}
