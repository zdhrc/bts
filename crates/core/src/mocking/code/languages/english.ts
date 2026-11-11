const _englishSpace = "[\\u0009-\\u000d\\u0020\\u0085\\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000]";
const _englishLeadingSpace = new RegExp("^" + _englishSpace + "+", "u");
const _englishTrailingSpace = new RegExp(_englishSpace + "+$", "u");
function _englishTrimStart(text: string): string { return text.replace(_englishLeadingSpace, ""); }
function _englishTrimEnd(text: string): string { return text.replace(_englishTrailingSpace, ""); }
function _englishTrim(text: string): string { return _englishTrimEnd(_englishTrimStart(text)); }

function _englishObject(value: any, fields: string[], defaults: any = {}): any {
    if (value === null || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some(key => !fields.includes(key))) throw new Error("invalid English object fields");
    return {...defaults, ...value};
}
function _englishEnum(value: any, choices: string[]): string {
    if (typeof value !== "string" || !choices.includes(value)) throw new Error("invalid English option");
    return value;
}
function _englishString(value: any): string {
    if (typeof value !== "string") throw new Error("expected English string");
    return value;
}
function _englishArray(value: any): any[] {
    if (!Array.isArray(value)) throw new Error("expected English array");
    return value;
}
function _englishFragment(value: any): string {
    value = _englishString(value);
    if (!value || _englishTrim(value) !== value || /[\x00-\x1f\x7f-\x9f?!;,\u2028\u2029]/u.test(value) || value.endsWith(".") || new RegExp("\\." + _englishSpace, "u").test(value)) throw new Error("invalid English fragment");
    return value;
}
function _englishSubject(value: any): any {
    value = _englishObject(value, ["text", "person", "number"], {person: "third", number: "singular"});
    const text = _englishFragment(value.text), person = _englishEnum(value.person, ["first", "second", "third"]), number = _englishEnum(value.number, ["singular", "plural"]);
    const expectedForms: any = ({I: ["first", "singular"], we: ["first", "plural"], We: ["first", "plural"], he: ["third", "singular"], He: ["third", "singular"], she: ["third", "singular"], She: ["third", "singular"], it: ["third", "singular"], It: ["third", "singular"], they: ["third", "plural"], They: ["third", "plural"]} as any);
    const expected = Object.hasOwn(expectedForms, text) ? expectedForms[text] : undefined;
    if (expected && (expected[0] !== person || expected[1] !== number) || ["you", "You"].includes(text) && person !== "second") throw new Error("pronoun agreement mismatch");
    return value;
}
function _englishVerb(value: any): any {
    if (typeof value === "string") {
        if (!Object.hasOwn(_englishData.verbs, value)) throw new Error("unknown English verb");
        return _englishData.verbs[value];
    }
    const fields = ["base", "third_person_singular", "past", "past_participle", "present_participle"];
    value = _englishObject(value, fields);
    if (fields.some(key => typeof value[key] !== "string" || !/^[a-z]+(?![\s\S])/.test(value[key]))) throw new Error("invalid English verb forms");
    if (value.base === "be" && fields.some(key => value[key] !== _englishData.verbs.be[key])) throw new Error("invalid forms of be");
    return value;
}
function _englishClause(value: any, condition = false): any {
    const fields = ["subject", "predicate", "polarity", "tense", "aspect", ...(condition ? [] : ["modal", "form"])];
    const clause = _englishObject(value, fields, {polarity: "positive", tense: "present", aspect: "simple"});
    clause.subject = _englishSubject(clause.subject);
    const predicate = _englishObject(clause.predicate, ["verb", "complement"]);
    predicate.verb = _englishVerb(predicate.verb);
    if (predicate.complement != null) _englishFragment(predicate.complement);
    clause.predicate = predicate;
    _englishEnum(clause.polarity, ["positive", "negative"]);
    _englishEnum(clause.tense, ["present", "past", "future"]);
    _englishEnum(clause.aspect, ["simple", "progressive", "perfect", "perfect_progressive"]);
    clause.form = condition ? "statement" : _englishEnum(clause.form === undefined ? "statement" : clause.form, ["statement", "question"]);
    clause.modal = condition ? null : clause.modal ?? null;
    if (clause.modal !== null) {
        _englishEnum(clause.modal, ["can", "could", "may", "might", "must", "should", "will", "would"]);
        if (clause.tense !== "present") throw new Error("modal clauses require present tense");
    }
    return clause;
}
function _englishSentence(value: any): any {
    const sentence = _englishObject(value, ["clause", "condition"]);
    sentence.clause = _englishClause(sentence.clause);
    if (sentence.condition != null) sentence.condition = _englishClause(sentence.condition, true);
    return sentence;
}
function _englishRenderOptions(value: any): any {
    const options = _englishObject(value, ["negation", "condition_position"], {negation: "expanded", condition_position: "before"});
    _englishEnum(options.negation, ["expanded", "contracted"]);
    _englishEnum(options.condition_position, ["before", "after"]);
    return options;
}
class _EnglishUnsupported extends Error {}
function _englishContract(auxiliary: string): string | undefined {
    return _englishData.negations.find((row: string[]) => row[0] === auxiliary)?.[2];
}
function _englishStart(text: string): string {
    const words = ["if", "the", "a", "an", "this", "that", "these", "those", "my", "your", "our", "their", "we", "you", "he", "she", "it", "they", "can", "could", "may", "might", "must", "should", "will", "would", "am", "is", "are", "was", "were", "have", "has", "had", "do", "does", "did", ..._englishData.negations.map((row: string[]) => row[2])];
    return words.includes(text.split(" ")[0]) ? text[0].toUpperCase() + text.slice(1) : text;
}
function _englishRenderClause(clause: any, style: string): string {
    const subject = clause.subject, predicate = clause.predicate, verb = predicate.verb;
    const third = subject.person === "third" && subject.number === "singular", past = clause.tense === "past";
    const be = past ? (["first", "third"].includes(subject.person) && subject.number === "singular" ? "was" : "were") : subject.person === "first" && subject.number === "singular" ? "am" : third ? "is" : "are";
    const have = past ? "had" : third ? "has" : "have", doWord = past ? "did" : third ? "does" : "do";
    const modal = clause.modal ?? (clause.tense === "future" ? "will" : null), aspect = clause.aspect;
    const negative = clause.polarity === "negative", question = clause.form === "question";
    let first: string, rest: string;
    if (modal !== null) {
        first = modal;
        rest = ({simple: verb.base, progressive: "be " + verb.present_participle, perfect: "have " + verb.past_participle, perfect_progressive: "have been " + verb.present_participle} as any)[aspect];
    } else if (aspect === "simple") {
        if (verb.base === "be") [first, rest] = [be, ""];
        else if (negative || question) [first, rest] = [doWord, verb.base];
        else [first, rest] = [past ? verb.past : third ? verb.third_person_singular : verb.base, ""];
    } else if (aspect === "progressive") [first, rest] = [be, verb.present_participle];
    else if (aspect === "perfect") [first, rest] = [have, verb.past_participle];
    else [first, rest] = [have, "been " + verb.present_participle];
    let start: string;
    if (negative && style === "contracted") {
        const contracted = _englishContract(first);
        if (contracted === undefined) throw new _EnglishUnsupported("unsupported negation contraction");
        start = question ? contracted + " " + subject.text : subject.text + " " + contracted;
    } else if (negative) start = question ? first + " " + subject.text + " not" : subject.text + (first === "can" ? " cannot" : " " + first + " not");
    else start = question ? first + " " + subject.text : subject.text + " " + first;
    return [start, rest, predicate.complement].filter(Boolean).join(" ");
}
function _englishRender(sentence: any, options: any, fragment = false): string {
    const main = _englishRenderClause(sentence.clause, options.negation);
    let body: string;
    if (sentence.condition != null) {
        const condition = _englishRenderClause(sentence.condition, options.negation);
        body = options.condition_position === "before" ? (fragment ? "if " : "If ") + condition + ", " + main : (fragment ? main : _englishStart(main)) + " if " + condition;
    } else body = fragment ? main : _englishStart(main);
    return fragment ? body : body + (sentence.clause.form === "question" ? "?" : ".");
}
function _englishSafeWord(word: string): boolean {
    return /^[\p{L}\p{N}_-]+(?![\s\S])/u.test(word) && /[\p{L}\p{N}]/u.test(word) && !["if", "when", "unless", "because", "although", "while", "that", "whether", "who", "whom", "whose", "which", "where", "as", "or", "and", "but", "not", "no", "never", "neither", "nor", "only", "just", "merely", "except", "rather", "instead", "so"].includes(word.toLowerCase());
}
function _englishDeterminer(word: string): boolean { return ["the", "a", "an", "this", "that", "these", "those", "my", "your", "our", "their", "his", "her", "its"].includes(word); }
function _englishAuxiliary(word: string): boolean { return ["can", "could", "may", "might", "must", "should", "will", "would", "do", "does", "did", "am", "is", "are", "was", "were", "have", "has", "had"].includes(word); }
function _englishAnalyzeSubject(words: string[]): boolean {
    if (!words.length) return false;
    if (["i", "you", "he", "she", "it", "we", "they"].includes(words[0].toLowerCase())) return words.length === 1;
    if (_englishDeterminer(words[0].toLowerCase())) return words.length >= 2 && words.slice(1).every(word => _englishSafeWord(word) && !_englishDeterminer(word.toLowerCase()) && !_englishAuxiliary(word.toLowerCase()) && !["i", "you", "he", "she", "it", "we", "they", "to"].includes(word.toLowerCase()));
    if (words.length === 1 && _englishSafeWord(words[0]) && /\p{L}/u.test(words[0]) && /[_0-9]/.test(words[0])) return true;
    return words.length <= 3 && words.every(word => _englishSafeWord(word) && /^\p{Lu}/u.test(word) && !_englishAuxiliary(word.toLowerCase()));
}
function _englishAgrees(auxiliary: string, words: string[]): boolean {
    if (words.length !== 1) return true;
    const agreementForms: any = ({i: ["am", "was", "do", "have"], you: ["are", "were", "do", "have"], we: ["are", "were", "do", "have"], they: ["are", "were", "do", "have"], he: ["is", "was", "does", "has"], she: ["is", "was", "does", "has"], it: ["is", "was", "does", "has"]} as any);
    const key = words[0].toLowerCase(), allowed = Object.hasOwn(agreementForms, key) ? agreementForms[key] : undefined;
    return allowed === undefined || !["am", "is", "are", "was", "were", "do", "does", "have", "has"].includes(auxiliary) || allowed.includes(auxiliary);
}
function _englishBeComplement(words: string[]): boolean {
    if (!words.length) return false;
    if (Object.values(_englishData.verbs).some((verb: any) => [verb.present_participle, verb.past_participle].includes(words[0]))) return true;
    if (_englishDeterminer(words[0]) || ["in", "on", "at", "near", "under", "above", "outside", "inside", "with", "without"].includes(words[0])) return words.length > 1;
    return ["ready", "late", "early", "available", "unavailable", "hungry", "thirsty", "open", "closed", "complete", "correct", "incorrect", "certain", "safe", "sure", "possible", "impossible", "necessary", "happy", "sad", "tired", "well", "ill", "here", "there", "alone", "able", "unable", "busy", "free", "valid", "invalid", "empty", "full", "active", "inactive", "online", "offline", "clear", "wrong", "right"].includes(words[0]);
}
function _englishPerfect(words: string[]): boolean { return words.length > 0 && (words[0] === "been" ? _englishBeComplement(words.slice(1)) : Object.values(_englishData.verbs).some((verb: any) => verb.past_participle === words[0])); }
function _englishBase(words: string[]): boolean {
    if (!words.length) return false;
    if (words[0] === "have" && words[1] === "to") return words.length > 2 && Object.hasOwn(_englishData.verbs, words[2]);
    return Object.hasOwn(_englishData.verbs, words[0]);
}
function _englishPredicate(auxiliary: string, words: string[]): boolean {
    if (!words.length || !words.every(_englishSafeWord)) return false;
    if (["do", "does", "did"].includes(auxiliary)) return _englishBase(words);
    if (["am", "is", "are", "was", "were"].includes(auxiliary)) return _englishBeComplement(words);
    if (["have", "has", "had"].includes(auxiliary)) return _englishPerfect(words);
    if (words[0] === "be") return _englishBeComplement(words.slice(1));
    if (words[0] === "have" && words.length > 1 && (words[1] === "been" || Object.values(_englishData.verbs).some((verb: any) => verb.past_participle === words[1]))) return _englishPerfect(words.slice(1));
    return _englishBase(words);
}
function _englishAnalyze(text: string): any {
    const trimmed = _englishTrim(text);
    if (!/[.?](?![\s\S])/.test(trimmed)) return null;
    const body = trimmed.slice(0, -1), question = trimmed.endsWith("?");
    if (/[\x00-\x08\x0a-\x1f\x7f-\x9f\u2028\u2029.?!]/u.test(body)) return null;
    const matches = [...body.matchAll(new RegExp("[^" + _englishSpace.slice(1, -1) + "]+", "gu"))], words = matches.map(match => match[0]);
    if (question) {
        if (!words.length || !_englishAuxiliary(words[0].toLowerCase())) return null;
        let found = 0;
        for (let end = 2; end < Math.min(words.length, 7); end++) if (_englishAnalyzeSubject(words.slice(1, end)) && _englishAgrees(words[0].toLowerCase(), words.slice(1, end)) && _englishPredicate(words[0].toLowerCase(), words.slice(end))) found++;
        return found === 1 ? {edit: null} : null;
    }
    let found: any = null;
    for (let index = 1; index < Math.min(words.length, 6); index++) {
        if (!_englishAnalyzeSubject(words.slice(0, index))) continue;
        const word = words[index], expansion = _englishData.negations.find((row: string[]) => row.slice(2).includes(word));
        let auxiliary: string, edit: any = null, start = index + 1;
        if (expansion) { auxiliary = expansion[0]; edit = [matches[index].index!, matches[index].index! + word.length, expansion[1]]; }
        else if (word === "cannot") { auxiliary = "can"; edit = [matches[index].index!, matches[index].index! + word.length, "can't"]; }
        else if (_englishAuxiliary(word)) {
            auxiliary = word;
            if (words[start] === "not") {
                if (word === "can") continue;
                const contracted = _englishContract(word);
                if (contracted !== undefined) edit = [matches[index].index!, matches[start].index! + words[start].length, contracted];
                start++;
            }
        } else continue;
        if (_englishAgrees(auxiliary, words.slice(0, index)) && _englishPredicate(auxiliary, words.slice(start))) {
            if (found !== null) return null;
            if (edit !== null) { const offset = text.length - _englishTrimStart(text).length; edit[0] += offset; edit[1] += offset; }
            found = {edit};
        }
    }
    return found ?? (_englishLexical(words) ? {edit: null} : null);
}
function _englishVary(value: any, options: any, random: () => number): string {
    options = _englishObject(options, ["negation", "condition_order", "rendering"], {negation: true, condition_order: true, rendering: {}});
    if (typeof options.negation !== "boolean" || typeof options.condition_order !== "boolean") throw new Error("variation controls must be booleans");
    const rendering = _englishRenderOptions(options.rendering);
    if (typeof value === "string") {
        const alternatives = _englishTextAlternatives(value, options);
        return alternatives.length ? [value, ...alternatives][Math.min(alternatives.length, Math.floor(random() * (alternatives.length + 1)))] : value;
    }
    const sentence = _englishSentence(value), original = _englishRender(sentence, rendering), alternatives = [original];
    const negative = sentence.clause.polarity === "negative" || sentence.condition != null && sentence.condition.polarity === "negative";
    const negativeQuestion = sentence.clause.form === "question" && sentence.clause.polarity === "negative";
    if (options.negation && negative && !negativeQuestion) {
        try { alternatives.push(_englishRender(sentence, {...rendering, negation: rendering.negation === "expanded" ? "contracted" : "expanded"})); }
        catch (error) { if (!(error instanceof _EnglishUnsupported)) throw error; }
    }
    if (options.condition_order && sentence.condition != null) alternatives.push(_englishRender(sentence, {...rendering, condition_position: rendering.condition_position === "before" ? "after" : "before"}));
    return alternatives.length > 1 ? alternatives[Math.min(alternatives.length - 1, Math.floor(random() * alternatives.length))] : original;
}
function _englishText(value: any): string {
    value = _englishString(value);
    if (!value || _englishTrim(value) !== value || /[\x00-\x1f\x7f-\x9f\u2028\u2029]/.test(value)) throw new Error("invalid context text");
    return value;
}
function _englishEntity(value: any): any {
    value = _englishObject(value, ["id", "label", "person", "number", "pronouns"], {person: "third", number: "singular", pronouns: null});
    _englishText(value.id); _englishText(value.label);
    _englishEnum(value.person, ["first", "second", "third"]); _englishEnum(value.number, ["singular", "plural"]);
    if (value.pronouns !== null) {
        const fields = ["subject", "object", "possessive_determiner", "possessive"], forms = _englishObject(value.pronouns, fields);
        if (fields.some(key => typeof forms[key] !== "string" || !/^[A-Za-z]+(?![\s\S])/.test(forms[key]))) throw new Error("invalid pronoun forms");
        value.pronouns = forms;
    }
    return value;
}
function _englishContext(value: any): any {
    value = _englishObject(value, ["entities", "mentioned", "facts"], {entities: [], mentioned: [], facts: []});
    value.entities = _englishArray(value.entities).map(_englishEntity);
    value.mentioned = _englishArray(value.mentioned).map(_englishText);
    value.facts = _englishArray(value.facts).map(_englishText);
    const ids = value.entities.map((entity: any) => entity.id);
    if (new Set(ids).size !== ids.length || new Set(value.mentioned).size !== value.mentioned.length || new Set(value.facts).size !== value.facts.length || value.mentioned.some((id: string) => !ids.includes(id))) throw new Error("invalid discourse context");
    return value;
}
function _englishEntityEqual(left: any, right: any): boolean {
    return ["id", "label", "person", "number"].every(key => left[key] === right[key]) && (left.pronouns === null ? right.pronouns === null : right.pronouns !== null && ["subject", "object", "possessive_determiner", "possessive"].every(key => left.pronouns[key] === right.pronouns[key]));
}
function _englishRefer(entity: any, context: any, options: any): string {
    entity = _englishEntity(entity); context = _englishContext(context);
    options = _englishObject(options, ["form", "style"], {form: "subject", style: "auto"});
    const form = _englishEnum(options.form, ["subject", "object", "possessive_determiner", "possessive"]), style = _englishEnum(options.style, ["auto", "name", "pronoun"]);
    if (context.entities.some((other: any) => other.id === entity.id && !_englishEntityEqual(other, entity))) throw new Error("conflicting entity definition");
    const candidate = entity.pronouns === null ? null : entity.pronouns[form];
    const unambiguous = candidate !== null && !context.entities.some((other: any) => other.id !== entity.id && context.mentioned.includes(other.id) && (other.pronouns === null || other.pronouns[form].toLowerCase() === candidate.toLowerCase()));
    if (style !== "name" && context.mentioned.includes(entity.id) && unambiguous) return candidate;
    if (style === "pronoun") throw new Error("ambiguous or unintroduced pronoun");
    return ["subject", "object"].includes(form) ? entity.label : entity.label + (/[sS](?![\s\S])/.test(entity.label) ? "'" : "'s");
}
function _englishRemember(context: any, entities: any, facts: any): any {
    context = _englishContext(context);
    for (const entity of _englishArray(entities).map(_englishEntity)) {
        const old = context.entities.find((other: any) => other.id === entity.id);
        if (old !== undefined && !_englishEntityEqual(old, entity)) throw new Error("conflicting entity definition");
        if (old === undefined) context.entities.push(entity);
        if (!context.mentioned.includes(entity.id)) context.mentioned.push(entity.id);
    }
    for (const fact of _englishArray(facts).map(_englishText)) if (!context.facts.includes(fact)) context.facts.push(fact);
    return context;
}
function _englishUnmentioned(details: any, context: any): any[] {
    context = _englishContext(context);
    const seen = new Map<string, string>(), selected = [];
    for (let detail of _englishArray(details)) {
        detail = _englishObject(detail, ["id", "text"]);
        const id = _englishText(detail.id), text = _englishText(detail.text);
        if (seen.has(id)) { if (seen.get(id) !== text) throw new Error("conflicting detail text"); continue; }
        seen.set(id, text);
        if (!context.facts.includes(id)) selected.push(detail);
    }
    return selected;
}
function _englishLocale(value: any): string { return _englishEnum(value, ["en-US", "en-GB", "en-CA", "en-AU", "en-IN"]); }
function _englishNumber(value: any, options: any): string {
    options = _englishObject(options, ["locale", "grouping", "fraction_digits"], {locale: "en-US", grouping: true, fraction_digits: null});
    const locale = _englishLocale(options.locale), precision = options.fraction_digits;
    if (typeof options.grouping !== "boolean" || precision !== null && (!Number.isInteger(precision) || precision < 0 || precision > 9)) throw new Error("invalid number options");
    if (typeof value === "number") { if (!Number.isFinite(value)) throw new Error("invalid number"); value = String(value); }
    value = _englishString(value);
    const match = /^([+-]?)([0-9]+)(?:\.([0-9]+))?(?:[eE]([+-]?[0-9]+))?(?![\s\S])/.exec(value);
    if (!match || value.length > 4096) throw new Error("invalid decimal");
    const sign = match[1], exponent = Number(match[4] ?? "0");
    if (!Number.isSafeInteger(exponent) || Math.abs(exponent) > 4096) throw new Error("decimal expansion too large");
    let integer = match[2], fraction = match[3] ?? "", digits = integer + fraction, point = integer.length + exponent;
    if (point <= 0 && 1 - point + digits.length > 4096 || point >= digits.length && point > 4096) throw new Error("decimal expansion too large");
    if (point < 0) { digits = "0".repeat(-point) + digits; point = 0; }
    if (point > digits.length) digits += "0".repeat(point - digits.length);
    integer = digits.slice(0, point).replace(/^0+/, "") || "0"; fraction = digits.slice(point);
    if (integer.length + fraction.length > 4096) throw new Error("decimal expansion too large");
    if (precision === null) fraction = fraction.replace(/0+(?![\s\S])/, "");
    else {
        const roundUp = fraction.length > precision && fraction[precision] >= "5";
        fraction = (fraction + "0".repeat(precision)).slice(0, precision);
        if (roundUp) {
            const combined = (integer + fraction).split("");
            let index = combined.length - 1;
            while (index >= 0 && combined[index] === "9") combined[index--] = "0";
            if (index >= 0) combined[index] = String(Number(combined[index]) + 1); else combined.unshift("1");
            const rounded = combined.join("");
            integer = precision ? rounded.slice(0, -precision) : rounded;
            fraction = precision ? rounded.slice(-precision) : "";
        }
    }
    const negative = sign === "-" && /[1-9]/.test(integer + fraction);
    if (options.grouping) {
        const parts = []; let width = 3;
        while (integer) { parts.push(integer.slice(-width)); integer = integer.slice(0, -width); if (locale === "en-IN") width = 2; }
        integer = parts.reverse().join(",");
    }
    return (negative ? "-" : "") + integer + (fraction ? "." + fraction : "");
}
function _englishDate(value: any, options: any): string {
    options = _englishObject(options, ["locale", "style"], {locale: "en-US", style: "long"});
    const locale = _englishLocale(options.locale), style = _englishEnum(options.style, ["short", "long"]);
    value = _englishString(value);
    if (!/^[0-9]{4}-[0-9]{2}-[0-9]{2}(?![\s\S])/.test(value)) throw new Error("invalid date");
    const [year, month, day] = value.split("-").map(Number), leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
    const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if (year < 1 || year > 9999 || month < 1 || month > 12 || day < 1 || day > days[month - 1]) throw new Error("invalid date");
    if (style === "short") { if (locale === "en-CA") return value; return locale === "en-US" ? `${String(month).padStart(2, "0")}/${String(day).padStart(2, "0")}/${String(year).padStart(4, "0")}` : `${String(day).padStart(2, "0")}/${String(month).padStart(2, "0")}/${String(year).padStart(4, "0")}`; }
    const name = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"][month - 1];
    return ["en-US", "en-CA"].includes(locale) ? `${name} ${day}, ${year}` : `${day} ${name} ${year}`;
}
function _englishTime(value: any, options: any): string {
    options = _englishObject(options, ["locale", "hour_cycle", "seconds"], {locale: "en-US", hour_cycle: null, seconds: false});
    const locale = _englishLocale(options.locale), cycle = options.hour_cycle === null ? locale === "en-GB" ? "h24" : "h12" : _englishEnum(options.hour_cycle, ["h12", "h24"]);
    if (typeof options.seconds !== "boolean") throw new Error("seconds must be boolean");
    value = _englishString(value);
    if (!/^[0-9]{2}:[0-9]{2}(?::[0-9]{2})?(?![\s\S])/.test(value)) throw new Error("invalid time");
    const parts = value.split(":").map(Number), [hour, minute] = parts, second = parts[2] ?? 0;
    if (hour > 23 || minute > 59 || second > 59) throw new Error("invalid time");
    let result = cycle === "h24" ? `${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}` : `${hour % 12 || 12}:${String(minute).padStart(2, "0")}`;
    if (options.seconds) result += `:${String(second).padStart(2, "0")}`;
    if (cycle === "h12") result += hour < 12 ? " AM" : " PM";
    return result;
}
function _language_english(operation: string, args: any[], random: () => number): any {
    const arities: any = {realize: [1, 2], vary: [1, 2], combine: [2, 3], refer: [2, 3], remember: [2, 3], unmentioned: [2, 2], format_number: [1, 2], format_date: [1, 2], format_time: [1, 2]};
    if (!Object.hasOwn(arities, operation) || !Array.isArray(args) || args.length < arities[operation][0] || args.length > arities[operation][1]) throw new Error("invalid English call");
    if (operation === "realize") return _englishRender(_englishSentence(args[0]), _englishRenderOptions(args.length > 1 ? args[1] : {}));
    if (operation === "vary") return _englishVary(args[0], args.length > 1 ? args[1] : {}, random);
    if (operation === "combine") {
        const sentences = _englishArray(args[0]).map(value => _englishSentence(value)), relation = _englishEnum(args[1], ["and", "but", "or", "because"]), options = _englishRenderOptions(args.length > 2 ? args[2] : {});
        if (sentences.length < 2 || relation === "because" && sentences.length !== 2 || sentences.some(sentence => sentence.clause.form !== "statement" || sentence.condition != null)) throw new Error("invalid proposition combination");
        if (relation === "because") return "Because " + _englishRender(sentences[1], options, true) + ", " + _englishRender(sentences[0], options, true) + ".";
        const separator = ", " + relation + " ";
        return _englishRender(sentences[0], options).slice(0, -1) + separator + sentences.slice(1).map(sentence => _englishRender(sentence, options, true)).join(separator) + ".";
    }
    if (operation === "refer") return _englishRefer(args[0], args[1], args.length > 2 ? args[2] : {});
    if (operation === "remember") return _englishRemember(args[0], args[1], args.length > 2 ? args[2] : []);
    if (operation === "unmentioned") return _englishUnmentioned(args[0], args[1]);
    if (operation === "format_number") return _englishNumber(args[0], args.length > 1 ? args[1] : {});
    if (operation === "format_date") return _englishDate(args[0], args.length > 1 ? args[1] : {});
    return _englishTime(args[0], args.length > 1 ? args[1] : {});
}
function _englishLexical(words: string[]): boolean {
    let found = 0;
    for (let index = 1; index < Math.min(words.length, 6); index++) {
        if (!_englishAnalyzeSubject(words.slice(0, index)) || !words.slice(index).every(_englishSafeWord)) continue;
        const pronoun = index === 1 ? words[0].toLowerCase() : null;
        const valid = Object.values(_englishData.verbs).some((verb: any) => verb.base !== "be" && (words[index] === verb.past || (["he", "she", "it"].includes(pronoun ?? "") ? words[index] === verb.third_person_singular : ["i", "you", "we", "they"].includes(pronoun ?? "") ? words[index] === verb.base : [verb.base, verb.third_person_singular].includes(words[index]))));
        if (valid) found++;
    }
    return found === 1;
}
function _englishInitialCase(text: string, question: boolean, upper: boolean): string {
    const end = text.indexOf(" ") === -1 ? text.length : text.indexOf(" "), first = text.slice(0, end), lower = first.toLowerCase(), tail = text.slice(end + 1);
    const pronoun = ["you", "we", "they", "he", "she", "it"].includes(lower), determiner = _englishDeterminer(lower) && /^\p{Ll}/u.test(tail), auxiliary = question && _englishAuxiliary(lower);
    if (!(pronoun || determiner || auxiliary) || !/^[\x00-\x7f]+(?![\s\S])/.test(first)) return text;
    return (upper ? text[0].toUpperCase() : text[0].toLowerCase()) + text.slice(1);
}
function _englishTextAlternatives(text: string, options: any): string[] {
    if (!options.negation && !options.condition_order) return [];
    const edits: any[] = [], trimmed = _englishTrim(text), leading = text.length - _englishTrimStart(text).length, trailing = _englishTrimEnd(text).length;
    let reordered: string | null = null, conditional: any = null;
    if (/[.?](?![\s\S])/.test(trimmed)) {
        const body = trimmed.slice(0, -1), punctuation = trimmed.slice(-1);
        if (!/[\x00-\x1f\x7f-\x9f\u2028\u2029.?!]/u.test(body)) {
            if ((body.startsWith("If ") || body.startsWith("if ")) && body.slice(3).includes(", ")) {
                const split = body.indexOf(", ", 3), condition = body.slice(3, split), main = body.slice(split + 2);
                if (condition && main && !main.includes(",")) conditional = {condition, main, conditionOffset: leading + 3, mainOffset: leading + split + 2, punctuation, before: true};
            } else if (body.includes(" if ")) {
                const split = body.indexOf(" if "), main = body.slice(0, split), condition = body.slice(split + 4);
                if (condition && main && !condition.includes(",") && !main.includes(",")) conditional = {condition, main, conditionOffset: leading + split + 4, mainOffset: leading, punctuation, before: false};
            }
        }
    }
    if (conditional !== null) {
        const {condition, main, conditionOffset, mainOffset, punctuation, before} = conditional;
        for (const [clause, ending, offset] of [[condition, ".", conditionOffset], [main, punctuation, mainOffset]]) {
            const analysis = _englishAnalyze(clause + ending);
            if (analysis === null) return [];
            if (analysis.edit !== null) { const [start, end, replacement] = analysis.edit; edits.push([offset + start, offset + end, replacement]); }
        }
        if (options.condition_order) {
            const adjusted = _englishInitialCase(main, punctuation === "?", before), changed = before ? adjusted + " if " + condition + punctuation : "If " + condition + ", " + adjusted + punctuation;
            reordered = text.slice(0, leading) + changed + text.slice(trailing);
        }
    } else {
        const segments = [...text.matchAll(new RegExp("[^.?]*(?:\\?" + _englishSpace + "*|\\.(?:" + _englishSpace + "+|$))", "gu"))];
        if (!segments.length || segments.map(segment => segment[0]).join("") !== text) return [];
        for (const [index, segment] of segments.entries()) {
            if (index && _englishTrimEnd(segments[index - 1][0]).endsWith(".") && /^\p{Lowercase}/u.test(segment[0]) && !/[\n\r\u0085\u2028\u2029]/u.test(segments[index - 1][0])) return [];
            const analysis = _englishAnalyze(segment[0]);
            if (analysis === null) return [];
            if (analysis.edit !== null) { const [start, end, replacement] = analysis.edit; edits.push([segment.index! + start, segment.index! + end, replacement]); }
        }
    }
    edits.sort((a, b) => a[0] - b[0]);
    const alternatives: string[] = [];
    if (options.negation) for (const contracts of [false, true]) {
        let changed = text;
        for (const [start, end, replacement] of [...edits].reverse()) {
            const targetContracts = !_englishData.negations.some((row: string[]) => row[1] === replacement);
            if (targetContracts === contracts) changed = changed.slice(0, start) + replacement + changed.slice(end);
        }
        if (changed !== text && !alternatives.includes(changed)) alternatives.push(changed);
    }
    if (reordered !== null && reordered !== text && !alternatives.includes(reordered)) alternatives.push(reordered);
    return alternatives;
}
