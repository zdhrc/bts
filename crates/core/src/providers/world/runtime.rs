//! The runtime the English functions share in generated code.
//!
//! Generated agents and scorers run the same functions data generation runs in
//! Rust. This is their text in Python and TypeScript, with the packaged vocabulary
//! and rules embedded as data. Every function goes through one entry point,
//! `_language_english(operation, args, random)`. The test below runs a fixed set of
//! calls through all three and compares what they return.

use crate::mocking::ScorerLang;

const PY_ENGLISH: &str = r#"_english_space = "\t\n\v\f\r \u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000"
_english_space_pattern = r"[\u0009-\u000d\u0020\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]"

def _english_object(value, fields, defaults=None):
    if type(value) is not dict or any(key not in fields for key in value):
        raise ValueError("invalid English object fields")
    result = dict(defaults or {})
    result.update(value)
    return result

def _english_enum(value, choices):
    if type(value) is not str or value not in choices:
        raise ValueError("invalid English option")
    return value

def _english_string(value):
    if type(value) is not str:
        raise ValueError("expected English string")
    return value

def _english_array(value):
    if type(value) is not list:
        raise ValueError("expected English array")
    return value

def _english_fragment(value):
    value = _english_string(value)
    if not value or value.strip(_english_space) != value or any(ord(c) < 32 or 127 <= ord(c) <= 159 or c in "?!;,\u2028\u2029" for c in value) or value.endswith(".") or _english_re.search(r"\." + _english_space_pattern, value):
        raise ValueError("invalid English fragment")
    return value

def _english_subject(value):
    value = _english_object(value, ["text", "person", "number"], {"person": "third", "number": "singular"})
    text = _english_fragment(value.get("text"))
    person = _english_enum(value["person"], ["first", "second", "third"])
    number = _english_enum(value["number"], ["singular", "plural"])
    expected = {"I": ("first", "singular"), "we": ("first", "plural"), "We": ("first", "plural"), "he": ("third", "singular"), "He": ("third", "singular"), "she": ("third", "singular"), "She": ("third", "singular"), "it": ("third", "singular"), "It": ("third", "singular"), "they": ("third", "plural"), "They": ("third", "plural")}.get(text)
    if expected is not None and expected != (person, number) or text in ["you", "You"] and person != "second":
        raise ValueError("pronoun agreement mismatch")
    return value

def _english_verb(value):
    if type(value) is str:
        if value not in _EnglishData.data["verbs"]:
            raise ValueError("unknown English verb")
        return _EnglishData.data["verbs"][value]
    fields = ["base", "third_person_singular", "past", "past_participle", "present_participle"]
    value = _english_object(value, fields)
    if any(type(value.get(key)) is not str or _english_re.fullmatch(r"[a-z]+", value[key]) is None for key in fields):
        raise ValueError("invalid English verb forms")
    if value["base"] == "be" and value != _EnglishData.data["verbs"]["be"]:
        raise ValueError("invalid forms of be")
    return value

def _english_clause(value, condition=False):
    fields = ["subject", "predicate", "polarity", "tense", "aspect"] + ([] if condition else ["modal", "form"])
    clause = _english_object(value, fields, {"polarity": "positive", "tense": "present", "aspect": "simple"})
    clause["subject"] = _english_subject(clause.get("subject"))
    predicate = _english_object(clause.get("predicate"), ["verb", "complement"])
    predicate["verb"] = _english_verb(predicate.get("verb"))
    if predicate.get("complement") is not None:
        _english_fragment(predicate["complement"])
    clause["predicate"] = predicate
    _english_enum(clause["polarity"], ["positive", "negative"])
    _english_enum(clause["tense"], ["present", "past", "future"])
    _english_enum(clause["aspect"], ["simple", "progressive", "perfect", "perfect_progressive"])
    clause["form"] = "statement" if condition else _english_enum(clause.get("form", "statement"), ["statement", "question"])
    clause["modal"] = None if condition else clause.get("modal")
    if clause["modal"] is not None:
        _english_enum(clause["modal"], ["can", "could", "may", "might", "must", "should", "will", "would"])
        if clause["tense"] != "present":
            raise ValueError("modal clauses require present tense")
    return clause

def _english_sentence(value):
    sentence = _english_object(value, ["clause", "condition"])
    sentence["clause"] = _english_clause(sentence.get("clause"))
    if sentence.get("condition") is not None:
        sentence["condition"] = _english_clause(sentence["condition"], True)
    return sentence

def _english_render_options(value):
    options = _english_object(value, ["negation", "condition_position"], {"negation": "expanded", "condition_position": "before"})
    _english_enum(options["negation"], ["expanded", "contracted"])
    _english_enum(options["condition_position"], ["before", "after"])
    return options

class _EnglishUnsupported(ValueError):
    pass

def _english_contract(auxiliary):
    return next((row[2] for row in _EnglishData.data["negations"] if row[0] == auxiliary), None)

def _english_start(text):
    first = text.split(" ")[0]
    ordinary = "a" <= first[:1] <= "z" and all("a" <= c <= "z" or c in "'-" for c in first)
    return text[0].upper() + text[1:] if ordinary else text

def _english_render_clause(clause, style):
    subject, predicate = clause["subject"], clause["predicate"]
    verb = predicate["verb"]
    third = subject["person"] == "third" and subject["number"] == "singular"
    past = clause["tense"] == "past"
    be = ("was" if subject["person"] in ["first", "third"] and subject["number"] == "singular" else "were") if past else ("am" if subject["person"] == "first" and subject["number"] == "singular" else "is" if third else "are")
    have = "had" if past else "has" if third else "have"
    do = "did" if past else "does" if third else "do"
    modal = clause["modal"] or ("will" if clause["tense"] == "future" else None)
    aspect = clause["aspect"]
    negative, question = clause["polarity"] == "negative", clause["form"] == "question"
    if modal is not None:
        first = modal
        rest = {"simple": verb["base"], "progressive": "be " + verb["present_participle"], "perfect": "have " + verb["past_participle"], "perfect_progressive": "have been " + verb["present_participle"]}[aspect]
    elif aspect == "simple":
        if verb["base"] == "be": first, rest = be, ""
        elif negative or question: first, rest = do, verb["base"]
        else: first, rest = verb["past"] if past else verb["third_person_singular"] if third else verb["base"], ""
    elif aspect == "progressive": first, rest = be, verb["present_participle"]
    elif aspect == "perfect": first, rest = have, verb["past_participle"]
    else: first, rest = have, "been " + verb["present_participle"]
    if negative and style == "contracted":
        first = _english_contract(first)
        if first is None: raise _EnglishUnsupported("unsupported negation contraction")
        start = first + " " + subject["text"] if question else subject["text"] + " " + first
    elif negative:
        start = first + " " + subject["text"] + " not" if question else subject["text"] + (" cannot" if first == "can" else " " + first + " not")
    else: start = first + " " + subject["text"] if question else subject["text"] + " " + first
    return " ".join(part for part in [start, rest, predicate.get("complement")] if part)

def _english_render(sentence, options, fragment=False):
    main = _english_render_clause(sentence["clause"], options["negation"])
    condition = sentence.get("condition")
    if condition is not None:
        condition = _english_render_clause(condition, options["negation"])
        body = ("if " if fragment else "If ") + condition + ", " + main if options["condition_position"] == "before" else (main if fragment else _english_start(main)) + " if " + condition
    else: body = main if fragment else _english_start(main)
    return body if fragment else body + ("?" if sentence["clause"]["form"] == "question" else ".")

def _english_safe_word(word):
    return bool(word) and all(c.isalnum() or c in "_-" for c in word) and any(c.isalnum() for c in word) and word.lower() not in ["if", "when", "unless", "because", "although", "while", "that", "whether", "who", "whom", "whose", "which", "where", "as", "or", "and", "but", "not", "no", "never", "neither", "nor", "only", "just", "merely", "except", "rather", "instead", "so"]

def _english_determiner(word):
    return word in ["the", "a", "an", "this", "that", "these", "those", "my", "your", "our", "their", "his", "her", "its"]

def _english_auxiliary(word):
    return word in ["can", "could", "may", "might", "must", "should", "will", "would", "do", "does", "did", "am", "is", "are", "was", "were", "have", "has", "had"]

def _english_analyze_subject(words):
    if not words: return False
    if words[0].lower() in ["i", "you", "he", "she", "it", "we", "they"]: return len(words) == 1
    if _english_determiner(words[0].lower()):
        return len(words) >= 2 and all(_english_safe_word(w) and not _english_determiner(w.lower()) and not _english_auxiliary(w.lower()) and w.lower() not in ["i", "you", "he", "she", "it", "we", "they", "to"] for w in words[1:])
    if len(words) == 1 and _english_safe_word(words[0]) and any(c.isalpha() for c in words[0]) and ("_" in words[0] or any(c in "0123456789" for c in words[0])): return True
    return len(words) <= 3 and all(_english_safe_word(w) and w[0].isupper() and not _english_auxiliary(w.lower()) for w in words)

def _english_agrees(auxiliary, words):
    if len(words) != 1: return True
    allowed = {"i": ["am", "was", "do", "have"], "you": ["are", "were", "do", "have"], "we": ["are", "were", "do", "have"], "they": ["are", "were", "do", "have"], "he": ["is", "was", "does", "has"], "she": ["is", "was", "does", "has"], "it": ["is", "was", "does", "has"]}.get(words[0].lower())
    return allowed is None or auxiliary not in ["am", "is", "are", "was", "were", "do", "does", "have", "has"] or auxiliary in allowed

def _english_be_complement(words):
    if not words: return False
    if any(words[0] in [v["present_participle"], v["past_participle"]] for v in _EnglishData.data["verbs"].values()): return True
    if _english_determiner(words[0]) or words[0] in ["in", "on", "at", "near", "under", "above", "outside", "inside", "with", "without"]: return len(words) > 1
    return words[0] in ["ready", "late", "early", "available", "unavailable", "hungry", "thirsty", "open", "closed", "complete", "correct", "incorrect", "certain", "safe", "sure", "possible", "impossible", "necessary", "happy", "sad", "tired", "well", "ill", "here", "there", "alone", "able", "unable", "busy", "free", "valid", "invalid", "empty", "full", "active", "inactive", "online", "offline", "clear", "wrong", "right"]

def _english_perfect(words):
    return bool(words) and (_english_be_complement(words[1:]) if words[0] == "been" else any(words[0] == v["past_participle"] for v in _EnglishData.data["verbs"].values()))

def _english_base(words):
    if not words: return False
    if words[0] == "have" and len(words) > 1 and words[1] == "to": return len(words) > 2 and words[2] in _EnglishData.data["verbs"]
    return words[0] in _EnglishData.data["verbs"]

def _english_predicate(auxiliary, words):
    if not words or not all(_english_safe_word(w) for w in words): return False
    if auxiliary in ["do", "does", "did"]: return _english_base(words)
    if auxiliary in ["am", "is", "are", "was", "were"]: return _english_be_complement(words)
    if auxiliary in ["have", "has", "had"]: return _english_perfect(words)
    if words[0] == "be": return _english_be_complement(words[1:])
    if words[0] == "have" and len(words) > 1 and (words[1] == "been" or any(words[1] == v["past_participle"] for v in _EnglishData.data["verbs"].values())): return _english_perfect(words[1:])
    return _english_base(words)

def _english_analyze(text):
    trimmed = text.strip(_english_space)
    if not trimmed.endswith((".", "?")): return None
    body, question = trimmed[:-1], trimmed[-1] == "?"
    if any((ord(c) < 32 and c != "\t") or 127 <= ord(c) <= 159 or c in "\u2028\u2029.?!" for c in body): return None
    matches = list(_english_re.finditer("[^" + _english_space_pattern[1:-1] + "]+", body))
    words = [m.group() for m in matches]
    if question:
        if not words or not _english_auxiliary(words[0].lower()): return None
        found = sum(_english_analyze_subject(words[1:end]) and _english_agrees(words[0].lower(), words[1:end]) and _english_predicate(words[0].lower(), words[end:]) for end in range(2, min(len(words), 7)))
        return {"edit": None} if found == 1 else None
    found = None
    for index in range(1, min(len(words), 6)):
        if not _english_analyze_subject(words[:index]): continue
        word, edit, start = words[index], None, index + 1
        expansion = next((row for row in _EnglishData.data["negations"] if word in row[2:]), None)
        if expansion is not None:
            auxiliary = expansion[0]
            edit = (matches[index].start(), matches[index].end(), expansion[1])
        elif word == "cannot": auxiliary, edit = "can", (matches[index].start(), matches[index].end(), "can't")
        elif _english_auxiliary(word):
            auxiliary = word
            if start < len(words) and words[start] == "not":
                if word == "can": continue
                contracted = _english_contract(word)
                if contracted is not None: edit = (matches[index].start(), matches[start].end(), contracted)
                start += 1
        else: continue
        if _english_agrees(auxiliary, words[:index]) and _english_predicate(auxiliary, words[start:]):
            if found is not None: return None
            if edit is not None:
                offset = len(text) - len(text.lstrip(_english_space))
                edit = (edit[0] + offset, edit[1] + offset, edit[2])
            found = {"edit": edit}
    return found if found is not None else {"edit": None} if _english_lexical(words) else None

def _english_vary(value, options, random):
    if type(options) is dict and any(k not in ["negation", "condition_order", "rendering"] for k in options): return _english_style(value, options, random)
    options = _english_object(options, ["negation", "condition_order", "rendering"], {"negation": True, "condition_order": True, "rendering": {}})
    if type(options["negation"]) is not bool or type(options["condition_order"]) is not bool: raise ValueError("variation controls must be booleans")
    rendering = _english_render_options(options["rendering"])
    if type(value) is str:
        alternatives = _english_text_alternatives(value, options)
        return ([value] + alternatives)[min(len(alternatives), int(random() * (len(alternatives) + 1)))] if alternatives else value
    sentence = _english_sentence(value)
    original = _english_render(sentence, rendering)
    alternatives = [original]
    negative = sentence["clause"]["polarity"] == "negative" or sentence.get("condition") is not None and sentence["condition"]["polarity"] == "negative"
    negative_question = sentence["clause"]["form"] == "question" and sentence["clause"]["polarity"] == "negative"
    if options["negation"] and negative and not negative_question:
        candidate = dict(rendering, negation="contracted" if rendering["negation"] == "expanded" else "expanded")
        try: alternatives.append(_english_render(sentence, candidate))
        except _EnglishUnsupported: pass
    if options["condition_order"] and sentence.get("condition") is not None:
        alternatives.append(_english_render(sentence, dict(rendering, condition_position="after" if rendering["condition_position"] == "before" else "before")))
    return alternatives[min(len(alternatives) - 1, int(random() * len(alternatives)))] if len(alternatives) > 1 else original

def _english_text(value):
    value = _english_string(value)
    if not value or value.strip(_english_space) != value or any(ord(c) < 32 or 127 <= ord(c) <= 159 or c in "\u2028\u2029" for c in value): raise ValueError("invalid context text")
    return value

def _english_entity(value):
    value = _english_object(value, ["id", "label", "person", "number", "pronouns"], {"person": "third", "number": "singular", "pronouns": None})
    _english_text(value.get("id")); _english_text(value.get("label"))
    _english_enum(value["person"], ["first", "second", "third"]); _english_enum(value["number"], ["singular", "plural"])
    if value["pronouns"] is not None:
        fields = ["subject", "object", "possessive_determiner", "possessive"]
        forms = _english_object(value["pronouns"], fields)
        if any(type(forms.get(key)) is not str or _english_re.fullmatch(r"[A-Za-z]+", forms[key]) is None for key in fields): raise ValueError("invalid pronoun forms")
        value["pronouns"] = forms
    return value

def _english_context(value):
    value = _english_object(value, ["entities", "mentioned", "facts"], {"entities": [], "mentioned": [], "facts": []})
    value["entities"] = [_english_entity(entity) for entity in _english_array(value["entities"])]
    value["mentioned"] = [_english_text(item) for item in _english_array(value["mentioned"])]
    value["facts"] = [_english_text(item) for item in _english_array(value["facts"])]
    ids = [entity["id"] for entity in value["entities"]]
    if len(set(ids)) != len(ids) or len(set(value["mentioned"])) != len(value["mentioned"]) or len(set(value["facts"])) != len(value["facts"]) or any(item not in ids for item in value["mentioned"]): raise ValueError("invalid discourse context")
    return value

def _english_refer(entity, context, options):
    entity, context = _english_entity(entity), _english_context(context)
    options = _english_object(options, ["form", "style"], {"form": "subject", "style": "auto"})
    form = _english_enum(options["form"], ["subject", "object", "possessive_determiner", "possessive"])
    style = _english_enum(options["style"], ["auto", "name", "pronoun"])
    if any(other["id"] == entity["id"] and other != entity for other in context["entities"]): raise ValueError("conflicting entity definition")
    candidate = entity["pronouns"][form] if entity["pronouns"] is not None else None
    unambiguous = candidate is not None and not any(other["id"] != entity["id"] and other["id"] in context["mentioned"] and (other["pronouns"] is None or other["pronouns"][form].lower() == candidate.lower()) for other in context["entities"])
    if style != "name" and entity["id"] in context["mentioned"] and unambiguous: return candidate
    if style == "pronoun": raise ValueError("ambiguous or unintroduced pronoun")
    if form in ["subject", "object"]: return entity["label"]
    return entity["label"] + ("'" if entity["label"].endswith(("s", "S")) else "'s")

def _english_remember(context, entities, facts):
    context = _english_context(context)
    for entity in [_english_entity(value) for value in _english_array(entities)]:
        old = next((other for other in context["entities"] if other["id"] == entity["id"]), None)
        if old is not None and old != entity: raise ValueError("conflicting entity definition")
        if old is None: context["entities"].append(entity)
        if entity["id"] not in context["mentioned"]: context["mentioned"].append(entity["id"])
    for fact in [_english_text(value) for value in _english_array(facts)]:
        if fact not in context["facts"]: context["facts"].append(fact)
    return context

def _english_unmentioned(details, context):
    context, seen, selected = _english_context(context), {}, []
    for detail in _english_array(details):
        detail = _english_object(detail, ["id", "text"])
        identifier, text = _english_text(detail.get("id")), _english_text(detail.get("text"))
        if identifier in seen:
            if seen[identifier] != text: raise ValueError("conflicting detail text")
            continue
        seen[identifier] = text
        if identifier not in context["facts"]: selected.append(detail)
    return selected

def _english_locale(value):
    return _english_enum(value, ["en-US", "en-GB", "en-CA", "en-AU", "en-IN"])

def _english_number(value, options):
    options = _english_object(options, ["locale", "grouping", "fraction_digits"], {"locale": "en-US", "grouping": True, "fraction_digits": None})
    locale = _english_locale(options["locale"])
    precision = options["fraction_digits"]
    if type(options["grouping"]) is not bool or precision is not None and (type(precision) not in [int, float] or not _english_math.isfinite(precision) or precision != int(precision) or not 0 <= precision <= 9): raise ValueError("invalid number options")
    if precision is not None: precision = int(precision)
    if type(value) in [int, float]:
        if not _english_math.isfinite(value): raise ValueError("invalid number")
        value = str(value)
    value = _english_string(value)
    match = _english_re.fullmatch(r"([+-]?)([0-9]+)(?:\.([0-9]+))?(?:[eE]([+-]?[0-9]+))?", value)
    if match is None or len(value) > 4096: raise ValueError("invalid decimal")
    sign, integer, fraction, exponent = match.groups()
    fraction, exponent = fraction or "", int(exponent or "0")
    if abs(exponent) > 4096: raise ValueError("decimal expansion too large")
    digits = integer + fraction
    point = len(integer) + exponent
    if point <= 0 and 1 - point + len(digits) > 4096 or point >= len(digits) and point > 4096: raise ValueError("decimal expansion too large")
    if point < 0: digits, point = "0" * -point + digits, 0
    if point > len(digits): digits += "0" * (point - len(digits))
    integer, fraction = (digits[:point].lstrip("0") or "0"), digits[point:]
    if len(integer) + len(fraction) > 4096: raise ValueError("decimal expansion too large")
    if precision is None: fraction = fraction.rstrip("0")
    else:
        round_up = len(fraction) > precision and fraction[precision] >= "5"
        fraction = (fraction + "0" * precision)[:precision]
        if round_up:
            combined = list(integer + fraction)
            index = len(combined) - 1
            while index >= 0 and combined[index] == "9": combined[index], index = "0", index - 1
            if index >= 0: combined[index] = str(int(combined[index]) + 1)
            else: combined.insert(0, "1")
            combined = "".join(combined)
            integer = combined[:-precision] if precision else combined
            fraction = combined[-precision:] if precision else ""
    negative = sign == "-" and any(c != "0" for c in integer + fraction)
    if options["grouping"]:
        parts, width = [], 3
        while integer:
            parts.append(integer[-width:]); integer = integer[:-width]
            if locale == "en-IN": width = 2
        integer = ",".join(reversed(parts))
    return ("-" if negative else "") + integer + ("." + fraction if fraction else "")

def _english_date(value, options):
    options = _english_object(options, ["locale", "style"], {"locale": "en-US", "style": "long"})
    locale, style = _english_locale(options["locale"]), _english_enum(options["style"], ["short", "long"])
    value = _english_string(value)
    if _english_re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", value) is None: raise ValueError("invalid date")
    year, month, day = map(int, value.split("-"))
    leap = year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)
    days = [31, 29 if leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    if not 1 <= year <= 9999 or not 1 <= month <= 12 or not 1 <= day <= days[month - 1]: raise ValueError("invalid date")
    if style == "short":
        if locale == "en-CA": return value
        return f"{month:02d}/{day:02d}/{year:04d}" if locale == "en-US" else f"{day:02d}/{month:02d}/{year:04d}"
    name = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"][month - 1]
    return f"{name} {day}, {year}" if locale in ["en-US", "en-CA"] else f"{day} {name} {year}"

def _english_time(value, options):
    options = _english_object(options, ["locale", "hour_cycle", "seconds"], {"locale": "en-US", "hour_cycle": None, "seconds": False})
    locale = _english_locale(options["locale"])
    cycle = ("h24" if locale == "en-GB" else "h12") if options["hour_cycle"] is None else _english_enum(options["hour_cycle"], ["h12", "h24"])
    if type(options["seconds"]) is not bool: raise ValueError("seconds must be boolean")
    value = _english_string(value)
    if _english_re.fullmatch(r"[0-9]{2}:[0-9]{2}(?::[0-9]{2})?", value) is None: raise ValueError("invalid time")
    parts = list(map(int, value.split(":")))
    hour, minute, second = parts[0], parts[1], parts[2] if len(parts) == 3 else 0
    if hour > 23 or minute > 59 or second > 59: raise ValueError("invalid time")
    result = f"{hour:02d}:{minute:02d}" if cycle == "h24" else f"{hour % 12 or 12}:{minute:02d}"
    if options["seconds"]: result += f":{second:02d}"
    if cycle == "h12": result += " AM" if hour < 12 else " PM"
    return result

def _language_english(operation, args, random):
    arities = {"express": (1,2), "expression": (1,2), "expression_candidates": (1,2), "realize": (1, 2), "vary": (1, 3), "combine": (2, 3), "refer": (2, 3), "remember": (2, 3), "unmentioned": (2, 2), "format_number": (1, 2), "format_date": (1, 2), "format_time": (1, 2)}
    if operation not in arities or type(args) is not list or not arities[operation][0] <= len(args) <= arities[operation][1]: raise ValueError("invalid English call")
    if operation in ["express","expression","expression_candidates"]: return _english_expression(operation, args[0], args[1] if len(args) > 1 else {}, random)
    if operation == "realize": return _english_render(_english_sentence(args[0]), _english_render_options(args[1] if len(args) > 1 else {}))
    if operation == "vary":
        if len(args) > 2:
            draws = iter(_english_array(args[2]))
            random = lambda: next(draws, 0.5)
        return _english_vary(args[0], args[1] if len(args) > 1 else {}, random)
    if operation == "combine":
        sentences = [_english_sentence(value) for value in _english_array(args[0])]
        relation = _english_enum(args[1], ["and", "but", "or", "because"])
        options = _english_render_options(args[2] if len(args) > 2 else {})
        if len(sentences) < 2 or relation == "because" and len(sentences) != 2 or any(sentence["clause"]["form"] != "statement" or sentence.get("condition") is not None for sentence in sentences): raise ValueError("invalid proposition combination")
        if relation == "because": return "Because " + _english_render(sentences[1], options, True) + ", " + _english_render(sentences[0], options, True) + "."
        separator = ", " + relation + " "
        return _english_render(sentences[0], options)[:-1] + separator + separator.join(_english_render(sentence, options, True) for sentence in sentences[1:]) + "."
    if operation == "refer": return _english_refer(args[0], args[1], args[2] if len(args) > 2 else {})
    if operation == "remember": return _english_remember(args[0], args[1], args[2] if len(args) > 2 else [])
    if operation == "unmentioned": return _english_unmentioned(args[0], args[1])
    if operation == "format_number": return _english_number(args[0], args[1] if len(args) > 1 else {})
    if operation == "format_date": return _english_date(args[0], args[1] if len(args) > 1 else {})
    return _english_time(args[0], args[1] if len(args) > 1 else {})

def _english_lexical(words):
    found = 0
    for index in range(1, min(len(words), 6)):
        if not _english_analyze_subject(words[:index]) or not all(_english_safe_word(word) for word in words[index:]): continue
        pronoun = words[0].lower() if index == 1 else None
        valid = False
        for verb in _EnglishData.data["verbs"].values():
            if verb["base"] == "be": continue
            forms = [verb["past"]]
            forms += [verb["third_person_singular"]] if pronoun in ["he", "she", "it"] else [verb["base"]] if pronoun in ["i", "you", "we", "they"] else [verb["base"], verb["third_person_singular"]]
            if words[index] in forms: valid = True
        found += int(valid)
    return found == 1

def _english_initial_case(text, question, upper):
    first, _, tail = text.partition(" ")
    lower = first.lower()
    pronoun = lower in ["you", "we", "they", "he", "she", "it"]
    determiner = _english_determiner(lower) and bool(tail) and tail[0].islower()
    auxiliary = question and _english_auxiliary(lower)
    if not (pronoun or determiner or auxiliary) or not first.isascii() or not first: return text
    return (text[0].upper() if upper else text[0].lower()) + text[1:]

def _english_text_alternatives(text, options):
    if not options["negation"] and not options["condition_order"]: return []
    edits, reordered = [], None
    trimmed, leading, trailing = text.strip(_english_space), len(text) - len(text.lstrip(_english_space)), len(text.rstrip(_english_space))
    conditional = None
    if trimmed.endswith((".", "?")):
        body, punctuation = trimmed[:-1], trimmed[-1]
        if not any(ord(c) < 32 or 127 <= ord(c) <= 159 or c in "\u2028\u2029.?!" for c in body):
            if body.startswith(("If ", "if ")) and ", " in body[3:]:
                condition, main = body[3:].split(", ", 1)
                if condition and main and "," not in main: conditional = (condition, main, leading + 3, leading + 3 + len(condition) + 2, punctuation, True)
            elif " if " in body:
                main, condition = body.split(" if ", 1)
                if condition and main and "," not in condition and "," not in main: conditional = (condition, main, leading + len(main) + 4, leading, punctuation, False)
    if conditional is not None:
        condition, main, condition_offset, main_offset, punctuation, before = conditional
        for clause, ending, offset in [(condition, ".", condition_offset), (main, punctuation, main_offset)]:
            analysis = _english_analyze(clause + ending)
            if analysis is None: return []
            if analysis["edit"] is not None:
                start, end, replacement = analysis["edit"]
                edits.append((offset + start, offset + end, replacement))
        if options["condition_order"]:
            main = _english_initial_case(main, punctuation == "?", before)
            changed = main + " if " + condition + punctuation if before else "If " + condition + ", " + main + punctuation
            reordered = text[:leading] + changed + text[trailing:]
    else:
        segments = list(_english_re.finditer(r"[^.?]*(?:\?" + _english_space_pattern + r"*|\.(?:" + _english_space_pattern + r"+|$))", text))
        if not segments or "".join(segment.group() for segment in segments) != text: return []
        for index, segment in enumerate(segments):
            if index and segments[index - 1].group().rstrip(_english_space).endswith(".") and segment.group()[0].islower() and not any(c in segments[index - 1].group() for c in "\n\r\u0085\u2028\u2029"): return []
            analysis = _english_analyze(segment.group())
            if analysis is None: return []
            if analysis["edit"] is not None:
                start, end, replacement = analysis["edit"]
                edits.append((segment.start() + start, segment.start() + end, replacement))
    edits.sort()
    alternatives = []
    if options["negation"]:
        for contracts in [False, True]:
            changed = text
            for start, end, replacement in reversed(edits):
                target_contracts = not any(row[1] == replacement for row in _EnglishData.data["negations"])
                if target_contracts == contracts: changed = changed[:start] + replacement + changed[end:]
            if changed != text and changed not in alternatives: alternatives.append(changed)
    if reordered is not None and reordered != text and reordered not in alternatives: alternatives.append(reordered)
    return alternatives

def _english_ascii_lower(text):
    return text.translate(str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz"))

def _english_style_options(options):
    data = _EnglishData.data['styles']
    options = _english_object(options, list(data['defaults']))
    style = options.get('style')
    if style is not None: _english_enum(style, list(data['presets']))
    result = dict(data['presets'][style] if style is not None else data['defaults'])
    optional = ['style', 'negation', 'condition_order', 'contractions', 'shorthand', 'casual', 'slang', 'reductions', 'fragments', 'warmth', 'soften_requests', 'punctuation', 'casing']
    result.update({k:v for k,v in options.items() if v is not None or k not in optional})
    for key in optional[1:-2]:
        if type(result[key]) is not bool: raise ValueError('style controls must be booleans')
    _english_enum(result['punctuation'], ['preserve', 'omit_period', 'exclamation', 'expressive'])
    _english_enum(result['casing'], ['preserve', 'lowercase'])
    result['rendering'] = _english_render_options(result['rendering'])
    result['typos'] = _english_object(result['typos'], ['probability', 'max_errors', 'kinds'], data['defaults']['typos'])
    for value in [result['probability'], result['typos']['probability']]:
        if type(value) not in [int, float] or not 0 <= value <= 1: raise ValueError('invalid style probability')
    for value in [result['max_changes'], result['typos']['max_errors']]:
        if type(value) not in [int, float] or not 0 <= value <= 16 or value != int(value): raise ValueError('invalid style budget')
    for kind in _english_array(result['typos']['kinds']): _english_enum(kind, ['common', 'omission', 'transposition', 'repetition'])
    result['protected'] = list(_english_array(result['protected']))
    if any(not _english_string(term) for term in result['protected']): raise ValueError('empty protected term')
    alternatives = []
    for family in _english_array(result['alternatives']):
        family = _english_object(family, ['from', 'to'])
        if not _english_string(family.get('from')).strip(_english_space) or not _english_array(family.get('to')) or any(not _english_string(value).strip(_english_space) for value in family['to']): raise ValueError('empty phrase alternatives')
        alternatives.append(family)
    result['alternatives'] = alternatives
    return result

def _english_style_protected(text, terms):
    ranges = []
    for term in terms:
        ranges.extend((m.start(), m.end()) for m in _english_re.finditer(_english_re.escape(term), text))
    quoted = None
    for i, char in enumerate(text):
        if quoted is not None:
            if char == quoted[1]: ranges.append((quoted[0], i+1)); quoted = None
        elif char in ['"', '`', '“', '‘'] or char == "'" and (i == 0 or not text[i-1].isalnum()):
            quoted = (i, {'“':'”', '‘':'’'}.get(char, char))
    if quoted is not None: ranges.append((quoted[0], len(text)))
    for match in _english_re.finditer('[^' + _english_space_pattern[1:-1] + ']+', text):
        core = match.group().rstrip('.,!?;)]')
        if any(c.isnumeric() or c in '@/\\_-:.={}' for c in core): ranges.append((match.start(), match.end()))
    return ranges

def _english_style_case(source, replacement):
    return replacement[:1].upper() + replacement[1:] if source[:1].isupper() and replacement and ord(replacement[0]) < 128 else replacement

def _english_style_boundary(text, start, end):
    def word(c): return ord(c) > 127 or c.isalnum() or c in "_'’-"
    return (start == 0 or not word(text[start-1])) and (end == len(text) or not word(text[end]))

def _english_style_start(text, start):
    before = text[:start].rstrip(_english_space)
    return not before or before[-1] in '.!?\n'

def _english_style_candidates(text, options):
    data = _EnglishData.data['styles']
    protected = _english_style_protected(text, options['protected'])
    edits = []
    def push(rule, start, end, replacement, stage):
        if text[start:end] != replacement and not any(start < b and a < end for a,b in protected):
            edit = {'rule':rule, 'start':start, 'end':end, 'replacement':replacement, 'stage':stage}
            if edit not in edits: edits.append(edit)
    lower = _english_ascii_lower(text)
    for rule in data['rules']:
        if not options[rule['category']]: continue
        for match in _english_re.finditer(_english_re.escape(_english_ascii_lower(rule['from'])), lower):
            start, stop = match.span()
            source = text[start:stop]
            if source not in [rule['from'], _english_style_case('A', rule['from']), _english_ascii_lower(rule['from'])]: continue
            initial = _english_style_start(text, start)
            if not rule['from'].endswith(' ') and not _english_style_boundary(text, start, stop): continue
            if rule['from'].endswith(' ') and not initial: continue
            tail = text[stop:]
            end = next((i for i,c in enumerate(tail) if c in '.?!\n'), len(tail))
            body = tail[:end]
            words = _english_re.findall('[^' + _english_space_pattern[1:-1] + ']+', body)
            full = not tail.strip(_english_space) or tail.startswith(('.', '!', '?'))
            guard = rule['guard']
            valid = False
            if guard == 'phrase': valid = True
            elif guard == 'initial': valid = initial
            elif guard == 'opening': valid = initial and (full or tail.startswith(','))
            elif guard == 'full': valid = initial and full
            elif guard == 'base': valid = initial and _english_predicate('will', words) and not any(w in ['if','whether'] for w in words) and (not rule['from'].endswith('going to') or not words or words[0] not in ['work','school','college','church','bed','sleep','shop','practice'])
            elif guard == 'progressive': valid = initial and _english_predicate('am', words) and tail[end:end+1] != '?' and bool(tail) and tail[0] in 'abcdefghijklmnopqrstuvwxyz' and bool(words) and any(words[0] == v['present_participle'] for v in _EnglishData.data['verbs'].values()) and not any(w in ['not','if','and','but','or','because'] for w in words)
            elif guard == 'request': valid = initial and bool(words) and tail[end:end+1] == '.' and not any(c in body for c in ',;:') and not any(w in ['if','and','or','not'] for w in words)
            elif guard.startswith('aux:'): valid = initial and _english_predicate(guard[4:], words)
            if not valid: continue
            replacement = _english_style_case(source, rule['to'])
            if guard == 'progressive': replacement = text[stop:stop+1].upper(); stop += 1
            if guard == 'request': replacement += body + '?'; stop += end + 1
            push(rule['id'], start, stop, replacement, rule['stage'])
    if options['negation']:
        for match in _english_re.finditer(r'[^.?!\n]*[.?!\n]|[^.?!\n]+$', text):
            sentence = match.group()
            analysis = _english_analyze(sentence)
            if analysis is not None and analysis['edit'] is not None:
                a,b,replacement = analysis['edit']
                if "'" in replacement: push('negation:contract', match.start()+a, match.start()+b, replacement, 0)
    if options['condition_order']:
        for alternative in _english_text_alternatives(text, {'negation':False, 'condition_order':True}): push('condition:order', 0, len(text), alternative, 0)
    for index, family in enumerate(options['alternatives']):
        for match in _english_re.finditer(_english_re.escape(family['from']), text):
            start, end = match.span()
            if _english_style_boundary(text, start, end):
                for choice, replacement in enumerate(family['to']): push(f'authored:{index}:{choice}', start, end, replacement, 1)
    end = len(text.rstrip(_english_space))
    if end:
        mark, before = text[end-1:end], text[:end-1]
        words = _english_re.findall('[^' + _english_space_pattern[1:-1] + ']+', before)
        last = words[-1] if words else ''
        terminal = mark == '.' and bool(last) and '.' not in last and _english_ascii_lower(last) not in data['abbreviations']
        punctuation = options['punctuation']
        if punctuation == 'omit_period' and terminal: push('punctuation:omit_period', end-1, end, '', 3)
        elif punctuation == 'exclamation' and terminal: push('punctuation:exclamation', end-1, end, '!', 3)
        elif punctuation == 'expressive' and (terminal or mark == '!' and not before.endswith('!')): push('punctuation:expressive', end-1, end, '!!', 3)
    if options['casing'] == 'lowercase':
        for pronoun in data['contractions']:
            for match in _english_re.finditer(_english_re.escape(pronoun), text):
                if _english_style_boundary(text, *match.span()): push('casing:lowercase', match.start(), match.end(), _english_ascii_lower(pronoun), 3)
    for match in _english_re.finditer(r'[A-Za-z]+', text):
        start,end = match.span()
        word, lower = match.group(), _english_ascii_lower(match.group())
        if lower not in data['words'] and lower not in data['case_words'] or not _english_style_boundary(text, start, end): continue
        if options['casing'] == 'lowercase': push('casing:lowercase', start, end, lower, 3)
        if lower not in data['words'] or word == 'could' or len(word) < 5 or word != lower or options['typos']['probability'] == 0: continue
        for kind in options['typos']['kinds']:
            replacement = None
            if kind == 'common': replacement = next((b for a,b in data['typos'] if a == word), None)
            elif kind == 'omission': replacement = word[:2] + word[3:]
            elif kind == 'transposition' and word[1] != word[2]: replacement = word[:1] + word[2] + word[1] + word[3:]
            elif kind == 'repetition': replacement = word[:2] + word[1] + word[2:]
            if replacement is not None and replacement not in data['words'] and not any(replacement in v.values() for v in _EnglishData.data['verbs'].values()): push('typo:'+kind, start, end, replacement, 4)
    return sorted(edits, key=lambda e:(e['stage'],e['start'],e['rule'],e['replacement']))

def _english_style(value, options, random, details=False):
    options = _english_style_options(options)
    if type(value) is str: text = value
    else:
        sentence = _english_sentence(value)
        text = _english_render(sentence, options['rendering'])
        for clause in [sentence['clause']] + ([sentence['condition']] if sentence.get('condition') is not None else []):
            subject = clause['subject']['text']
            if _english_ascii_lower(subject) not in ['i','you','he','she','it','we','they']:
                options['protected'] += [subject, _english_style_case('A', subject)]
    budget, edits, gate = int(options['max_changes']), [], None
    for stage in range(5):
        if budget == 0: break
        candidates = [e for e in _english_style_candidates(text, options) if e['stage'] == stage]
        if not candidates: continue
        limit = budget
        if stage < 4:
            if gate is None: gate = options['probability'] == 1 or options['probability'] > 0 and random() < options['probability']
            if not gate: continue
        else:
            limit = min(budget, int(options['typos']['max_errors']))
            probability = options['typos']['probability']
            if limit == 0 or probability == 0 or probability < 1 and random() >= probability: continue
        for _ in range(limit):
            if not candidates: break
            index = 0 if len(candidates) == 1 else min(len(candidates)-1, int(random()*len(candidates)))
            edit = candidates.pop(index)
            start,end,replacement = edit['start'],edit['end'],edit['replacement']
            candidates = [e for e in candidates if not (start < e['end'] and e['start'] < end)]
            difference = len(replacement) - (end-start)
            for other in candidates:
                if other['start'] >= end: other['start'] += difference; other['end'] += difference
            edits.append({'rule':edit['rule'],'range':{'start':len(text[:start].encode('utf-8')),'end':len(text[:end].encode('utf-8'))},'replacement':replacement,'stage':edit['stage']})
            text = text[:start] + replacement + text[end:]
            budget -= 1
    return {'text':text,'edits':edits} if details else text

def _english_expression_ids(values):
    values = _english_array(values)
    for value in values: _english_text(value)
    if len(set(values)) != len(values): raise ValueError('duplicate expression identifiers')
    return values

def _english_expression_weight(value, maximum=1000000):
    if type(value) not in [int,float] or not _english_math.isfinite(value) or not 0 <= value <= maximum: raise ValueError('invalid expression weight or probability')
    return value

def _english_expression_input(source, options):
    source = _english_object(source, ['intent','families','constructions','facts','optional_details','protected','topic','components'], {'constructions':[],'facts':[],'optional_details':[],'protected':[],'topic':None,'components':[]})
    _english_text(source.get('intent'))
    source['constructions'] = [_english_object(c,['id','predicate','weight','requires','excludes','mentions'],{'weight':1,'requires':[],'excludes':[],'mentions':[]}) for c in _english_array(source['constructions'])]
    for c in source['constructions']:
        _english_text(c.get('id')); _english_expression_weight(c['weight'])
        c['predicate'] = _english_object(c.get('predicate'), ['verb','complement'], {'complement':None})
        c['predicate']['verb'] = _english_verb(c['predicate'].get('verb'))
        if c['predicate']['complement'] is not None: _english_fragment(c['predicate']['complement'])
        for key in ['requires','excludes','mentions']: _english_expression_ids(c[key])
        if any(i in c['excludes'] for i in c['requires']): raise ValueError('conflicting construction conditions')
    ids = _english_expression_ids([c['id'] for c in source['constructions']])
    source['facts'] = [_english_object(f,['id','text']) for f in _english_array(source['facts'])]
    for fact in source['facts']: _english_text(fact.get('id')); _english_text(fact.get('text'))
    facts = _english_expression_ids([f['id'] for f in source['facts']])
    if any(i not in facts for i in _english_expression_ids(source['optional_details'])): raise ValueError('unknown optional detail')
    if any(not _english_string(s) for s in _english_array(source['protected'])): raise ValueError('empty protected term')
    fields = ['id','kind','weight','constructions','patterns','requires','excludes','context_requires','establishes','mentions','reason','text','message']
    defaults = {'weight':1,'constructions':[],'patterns':[],'requires':[],'excludes':[],'context_requires':[],'establishes':[],'mentions':[],'reason':None,'text':None,'message':None}
    source['families'] = [_english_object(f,fields,defaults) for f in _english_array(source.get('families'))]
    if not source['families']: raise ValueError('no expression families')
    _english_expression_ids([f.get('id') for f in source['families']])
    for f in source['families']:
        _english_enum(f.get('kind'), list(_EnglishData.data['expressions']['patterns']))
        _english_expression_weight(f['weight'])
        for key in ['constructions','patterns','requires','excludes','context_requires','establishes','mentions']: _english_expression_ids(f[key])
        if any(i not in ids for i in f['constructions']) or any(i in f['excludes'] for i in f['requires']): raise ValueError('invalid family conditions')
        if f['kind'] == 'structured':
            if f['message'] is None or f['text'] is not None or f['reason'] is not None or f['constructions'] or f['patterns']: raise ValueError('invalid structured family')
        elif f['kind'] in ['authored','follow_up']:
            _english_text(f['text'])
            if f['reason'] is not None or f['constructions'] or f['patterns'] or f['kind'] == 'follow_up' and not f['context_requires']: raise ValueError('invalid authored family')
        else:
            if f['text'] is not None or (f['kind'] == 'reason_request') != (f['reason'] is not None): raise ValueError('invalid family text or reason')
            if f['reason'] is not None: _english_text(f['reason'])
            if any(i not in [p['id'] for p in _EnglishData.data['expressions']['patterns'][f['kind']]] for i in f['patterns']): raise ValueError('unknown expression pattern')
    defaults = _EnglishData.data['expressions']['options']
    options = _english_object(options,['speaker','context','repeat_penalty'],defaults)
    speaker = _english_object(options['speaker'],list(defaults['speaker']),defaults['speaker'])
    speaker['wording'] = _english_style_options(speaker['wording'])
    for key in ['family_weights','pattern_weights']:
        if type(speaker[key]) is not dict: raise ValueError('expected expression weights')
        for name,weight in speaker[key].items(): _english_text(name); _english_expression_weight(weight)
    for key in ['greetings','closings']:
        speaker[key] = [_english_object(p,['text','weight'],{'weight':1}) for p in _english_array(speaker[key])]
        for phrase in speaker[key]: _english_text(phrase.get('text')); _english_expression_weight(phrase['weight'])
    for key in ['greeting_probability','closing_probability','detail_probability']: _english_expression_weight(speaker[key],1)
    value = speaker['max_details']
    if type(value) not in [int,float] or not 0 <= value <= 16 or value != int(value): raise ValueError('invalid detail budget')
    options['speaker'] = speaker
    _english_expression_weight(options['repeat_penalty'],1)
    context = _english_object(options['context'],['established','recent','topic','known','requested','offered','allowed_acts'],{'established':[],'recent':[],'topic':None,'known':[],'requested':[],'offered':[],'allowed_acts':[]})
    _english_expression_ids(context['established'])
    context['recent'] = [_english_object(r,['intent','family','construction','pattern','message']) for r in _english_array(context['recent'])]
    if len(context['recent']) > 32: raise ValueError('history exceeds 32 selections')
    for r in context['recent']:
        for key in ['intent','family','pattern']: _english_text(r.get(key))
        if r.get('construction') is not None: _english_text(r['construction'])
        r.setdefault('construction',None)
    options['context'] = context
    _english_message_input(source,options)
    return source,options

def _english_expression_complete(text):
    return text if text.endswith(('.', '?', '!')) else text+'.'

def _english_expression_candidates(source, options):
    facts = {f['id']:f['text'] for f in source['facts']}
    def applies(value): return all(i in facts for i in value['requires']+value['mentions']) and all(i not in facts for i in value['excludes'])
    recent = [r for r in options['context']['recent'] if r['intent'] == source['intent']]
    result, family_total = [], 0.0
    speaker = options['speaker']
    for f in source['families']:
        if not applies(f) or not all(i in options['context']['established'] for i in f['context_requires']) or f['reason'] is not None and f['reason'] not in facts: continue
        fw = f['weight'] * speaker['family_weights'].get(f['id'],1) * (options['repeat_penalty'] if any(r['family'] == f['id'] for r in recent) else 1)
        if f['message'] is not None:
            layout = _EnglishData.data['expressions']['messages']['layouts'][f['message']['layout']]
            elaboration = _english_message_elaboration(f['message'],layout)
            fw *= speaker['organization'].get(layout['organization'],1)*speaker['elaboration'].get(elaboration,1)
        if fw == 0: continue
        used = list(dict.fromkeys(f['mentions'] + ([f['reason']] if f['reason'] is not None else [])))
        group, total = [], 0.0
        def candidate(construction,pattern,text,mentions,weight):
            return {'selection':{'intent':source['intent'],'family':f['id'],'construction':construction,'pattern':pattern},'text':text,'used_facts':mentions,'establishes':list(f['establishes']),'probability':weight}
        if f['kind'] in ['authored','follow_up','structured']:
            if speaker['pattern_weights'].get(f['kind'],1) > 0:
                total = 1.0
                group.append(candidate(None,f['kind'],f['text'] or '',used,1.0))
        else:
            patterns = [(p,speaker['pattern_weights'].get(p['id'],1)) for p in _EnglishData.data['expressions']['patterns'][f['kind']] if (not f['patterns'] or p['id'] in f['patterns']) and speaker['pattern_weights'].get(p['id'],1) > 0]
            pattern_total = sum(w for p,w in patterns)
            if pattern_total == 0: continue
            for c in source['constructions']:
                if f['constructions'] and c['id'] not in f['constructions'] or not applies(c): continue
                weight = c['weight'] * (options['repeat_penalty'] if any(r['construction'] == c['id'] for r in recent) else 1)
                if weight == 0: continue
                total += weight
                mentions = list(dict.fromkeys(c['mentions']+used))
                for pattern,pw in patterns:
                    predicate = c['predicate']
                    action = predicate['verb']['present_participle' if pattern['gerund'] else 'base'] + (' '+predicate['complement'] if predicate['complement'] is not None else '')
                    text = pattern['prefix']+action+pattern['suffix']
                    if f['reason'] is not None: text = _english_expression_complete(facts[f['reason']])+' '+text
                    group.append(candidate(c['id'],pattern['id'],text,mentions,weight*pw/pattern_total))
        if f['message'] is not None:
            group = _english_message_expand(source,f,options,group)
            total = sum(c['probability'] for c in group)
        if total > 0:
            family_total += fw
            for c in group: c['probability'] = c['probability']/total*fw
            result += group
    for c in result:
        c['probability'] /= family_total
        if c['probability'] in [0,1]: c['probability'] = int(c['probability'])
    return result

def _english_expression_pick(weights, random):
    if len(weights) == 1: return 0
    draw = random()*sum(weights)
    for i,weight in enumerate(weights):
        if draw < weight: return i
        draw -= weight
    return len(weights)-1

def _english_expression(operation, source, options, random):
    source, options = _english_expression_input(source,options)
    candidates = _english_expression_candidates(source,options)
    if operation == 'expression_candidates': return candidates
    if not candidates: raise ValueError('no eligible expression with positive weight')
    chosen = candidates[_english_expression_pick([c['probability'] for c in candidates],random)]
    speaker, context = options['speaker'], options['context']
    def chance(p): return p == 1 or p > 0 and random() < p
    def phrase(key,p):
        values = [v for v in speaker[key] if v['weight'] > 0]
        return values[_english_expression_pick([v['weight'] for v in values],random)]['text'] if values and chance(p) else None
    parts = []
    greeting = phrase('greetings',speaker['greeting_probability']) if not context['recent'] else None
    if greeting is not None: parts.append(greeting)
    parts.append(chosen['text'])
    used = list(chosen['used_facts'])
    details = [i for i in source['optional_details'] if i not in used and i not in context['established']]
    for _ in range(int(speaker['max_details'])):
        if not details or not chance(speaker['detail_probability']): break
        id = details.pop(_english_expression_pick([1.0]*len(details),random))
        parts.append(_english_expression_complete(next(f['text'] for f in source['facts'] if f['id'] == id)))
        used.append(id)
    closing = phrase('closings',speaker['closing_probability'])
    if closing is not None: parts.append(closing)
    base = ' '.join(parts)
    wording = dict(speaker['wording'],protected=speaker['wording']['protected']+source['protected'])
    if 'message' in chosen['selection']: wording['fragments'] = False
    styled = _english_style(base,wording,random,True)
    result = {'text':styled['text'],'base_text':base,'selection':chosen['selection'],'used_facts':used,'edits':styled['edits'],'context':{'established':list(dict.fromkeys(context['established']+chosen['establishes']+used)),'recent':(context['recent']+[chosen['selection']])[-32:]}}
    result['context'] = _english_message_context(source,chosen,used,context,result['context'])
    return result['text'] if operation == 'express' else result

def _english_message_elaboration(plan, layout):
    return 'detailed' if plan['details'] else 'minimal' if layout['act'] is not None or plan['layout'] == 'request_only' else 'relevant_context'

def _english_message_input(source, options):
    data = _EnglishData.data['expressions']['messages']
    context = options['context']
    for topic in [source['topic'],context['topic']]:
        if topic is not None: _english_text(topic)
    for key in ['known','requested','offered','allowed_acts']: _english_expression_ids(context[key])
    for act in context['allowed_acts']: _english_enum(act,['answer','correction','acknowledgment','clarification','follow_up_request'])
    for key,values in data['preferences'].items():
        weights = options['speaker'][key]
        if type(weights) is not dict: raise ValueError('invalid message preferences')
        for name,weight in weights.items(): _english_enum(name,values); _english_expression_weight(weight)
    source['components'] = [_english_object(c,['id','role','forms','mentions','requires','context_requires','slot','option'],{'mentions':[],'requires':[],'context_requires':[],'slot':None,'option':None}) for c in _english_array(source['components'])]
    _english_expression_ids([c.get('id') for c in source['components']])
    for c in source['components']:
        _english_enum(c.get('role'),data['roles'])
        for key in ['mentions','requires','context_requires']: _english_expression_ids(c[key])
        for key in ['slot','option']:
            if c[key] is not None: _english_text(c[key])
        c['forms'] = [_english_object(f,['id','text','shape','reference','weight','context_requires'],{'shape':'complete','reference':'explicit','weight':1,'context_requires':[]}) for f in _english_array(c.get('forms'))]
        if not c['forms']: raise ValueError('empty component forms')
        _english_expression_ids([f.get('id') for f in c['forms']])
        for f in c['forms']:
            _english_text(f.get('text')); _english_expression_weight(f['weight']); _english_expression_ids(f['context_requires'])
            _english_enum(f['shape'],['complete','fragments','clause','value']); _english_enum(f['reference'],['explicit','contextual'])
            if f['reference'] == 'contextual' and (source['topic'] is None or not c['context_requires'] and not f['context_requires']) or f['shape'] != 'complete' and f['text'].endswith(('.', '?', '!')): raise ValueError('invalid component grammar or reference guard')
    components = {c['id']:c for c in source['components']}
    for f in source['families']:
        if f['message'] is None: continue
        p = _english_object(f['message'],['layout','bindings','relation','allow_apology','details'],{'bindings':{},'relation':None,'allow_apology':False,'details':[]})
        f['message'] = p
        _english_enum(p.get('layout'),list(data['layouts']))
        layout = data['layouts'][p['layout']]
        if type(p['allow_apology']) is not bool: raise ValueError('invalid apology flag')
        _english_expression_ids(p['details'])
        if type(p['bindings']) is not dict or set(p['bindings']) != set(layout['roles']) or (f['kind'] == 'structured') != layout['standalone'] or f['kind'] == 'reason_request' or source['optional_details'] or p['allow_apology'] and p['layout'] != 'correction': raise ValueError('incompatible message layout')
        if p['relation'] is not None and (p['layout'],p['relation']) not in [('request_reason','cause'),('reason_request','cause'),('current_desired','contrast')]: raise ValueError('invalid message relationship')
        for role,id in p['bindings'].items():
            _english_text(id)
            if id not in components or components[id]['role'] != role: raise ValueError('invalid message binding')
        for id in p['details']:
            if id not in components or components[id]['role'] != 'detail': raise ValueError('invalid message detail')
        if layout['act'] is not None and source['topic'] is None: raise ValueError('responses require a topic')
        if p['layout'] in ['answer','clarification','correction']:
            bound = [components[id] for id in p['bindings'].values()]
            if any(c['slot'] is None for c in bound) or len(set(c['slot'] for c in bound)) != 1 or p['layout'] == 'correction' and (any(not c['mentions'] for c in bound) or set(bound[0]['mentions']) & set(bound[1]['mentions'])): raise ValueError('invalid response slots or facts')
    for r in context['recent']:
        if r.get('message') is None: r.pop('message',None)
        if r.get('message') is not None:
            m = _english_object(r['message'],['layout','recipe','forms','organization','elaboration','sentence_shape','reference_style'])
            for key in ['layout','recipe','organization','elaboration','sentence_shape','reference_style']: _english_text(m.get(key))
            _english_expression_ids(m.get('forms'))

def _english_message_expand(source, family, options, bases):
    data = _EnglishData.data['expressions']['messages']
    plan, context, speaker = family['message'], options['context'], options['speaker']
    layout = data['layouts'][plan['layout']]
    topic_matches = source['topic'] is not None and source['topic'] == context['topic']
    if layout['act'] is not None and (not topic_matches or layout['act'] not in context['allowed_acts']): return []
    components = {c['id']:c for c in source['components']}
    for role,id in plan['bindings'].items():
        c = components[id]
        if plan['layout'] in ['answer','clarification'] and c['slot'] not in context['requested'] or role == 'previous' and not all(i in context['established'] for i in c['mentions']): return []
    facts = [f['id'] for f in source['facts']]+context['known']
    def applies(c): return all(i in facts for i in c['mentions']+c['requires']) and all(i in context['established'] for i in c['context_requires']) and (c['option'] is None or topic_matches and c['option'] in context['offered'])
    organization, elaboration = layout['organization'], _english_message_elaboration(plan,layout)
    preference = speaker['organization'].get(organization,1)*speaker['elaboration'].get(elaboration,1)
    if preference == 0: return []
    result = []
    for base in bases:
        expanded = []
        for recipe in data['recipes']:
            if recipe['layout'] != plan['layout'] or 'relation' in recipe and recipe['relation'] != plan['relation'] or recipe.get('apology',False) and not plan['allow_apology'] or recipe.get('declarative',False) and base['selection']['pattern'] not in ['desire_would_like','desire_want','desire_looking_to','need_need_to']: continue
            choices = [(list(base['used_facts']),[],recipe.get('reference') == 'contextual',1.0)]
            slots = [(role,components[plan['bindings'][role]],recipe['forms'][role]) for role in layout['roles']]+[('detail',components[id],'complete') for id in plan['details']]
            for role,c,shape in slots:
                if not applies(c): choices = []; break
                forms = [f for f in c['forms'] if f['shape'] == shape and f['weight'] > 0 and not (role == 'current' and recipe.get('relation') == 'contrast' and f['text'].endswith(('?', '!'))) and all(i in context['established'] for i in f['context_requires']) and (f['reference'] != 'contextual' or topic_matches)]
                total = sum(f['weight'] for f in forms)
                if len(choices)*len(forms) > 16384: raise ValueError('message expansion exceeds 16384 choices')
                following = []
                for used,selected,contextual,w in choices:
                    for f in forms:
                        following.append((list(dict.fromkeys(used+c['mentions'])),selected+[_english_message_form_id(c['id'],f['id'])],contextual or f['reference'] == 'contextual',w*f['weight']/total))
                choices = following
            shape = 'short_sentences' if plan['details'] and recipe['shape'] == 'complete' else recipe['shape']
            for used,forms,contextual,w in choices:
                text = _english_message_render(recipe,plan,components,layout,base['text'],forms)
                if contextual and not topic_matches: continue
                reference = 'contextual' if contextual else 'explicit'
                probability = w*speaker['sentence_shape'].get(shape,1)*speaker['reference_style'].get(reference,1)
                if probability == 0: continue
                selection = dict(base['selection'],message={'layout':plan['layout'],'recipe':recipe['id'],'forms':forms,'organization':organization,'elaboration':elaboration,'sentence_shape':shape,'reference_style':reference})
                expanded.append(dict(base,text=text,used_facts=used,selection=selection,probability=probability))
        total = sum(c['probability'] for c in expanded)
        for c in expanded: c['probability'] = c['probability']/total*base['probability']*preference
        result += expanded
        if len(result) > 16384: raise ValueError('message expansion exceeds 16384 choices')
    return result

def _english_message_context(source, chosen, used, previous, context):
    for key in ['topic','known','requested','offered','allowed_acts']: context[key] = previous[key] if key == 'topic' else list(previous[key])
    if source['topic'] is not None:
        if context['topic'] != source['topic']:
            for key in ['requested','offered','allowed_acts']: context[key] = []
        context['topic'] = source['topic']
        context['known'] = list(dict.fromkeys(context['known']+used))
    if 'message' in chosen['selection']:
        plan = next(f['message'] for f in source['families'] if f['id'] == chosen['selection']['family'])
        def component(role): return next(c for c in source['components'] if c['id'] == plan['bindings'][role])
        if plan['layout'] == 'answer': context['requested'] = [i for i in context['requested'] if i != component('answer')['slot']]
        if plan['layout'] == 'correction': context['known'] = [i for i in context['known'] if i not in component('previous')['mentions']]
        if _EnglishData.data['expressions']['messages']['layouts'][plan['layout']]['act'] is not None: context['allowed_acts'] = []
    return {k:v for k,v in context.items() if k in ['established','recent'] or v is not None and v != []}

def _english_message_render(recipe, plan, components, layout, request, forms):
    values = {'request':request,'request_clause':request.rstrip('.')}
    bindings = [(r,plan['bindings'][r]) for r in layout['roles']]+[('detail',id) for id in plan['details']]
    details = []
    for (role,id),selected in zip(bindings,forms):
        form = next(f for f in components[id]['forms'] if _english_message_form_id(id,f['id']) == selected)
        value = _english_expression_complete(form['text']) if form['shape'] == 'complete' else form['text']
        if role == 'detail': details.append(value)
        else: values[role] = value; values[role+'_clause'] = value.rstrip('.')
    remaining, rendered = recipe['text'], ''
    while '{' in remaining:
        prefix,rest = remaining.split('{',1)
        key,remaining = rest.split('}',1)
        rendered += prefix+values[key]
    return ' '.join([rendered+remaining]+details)

def _english_message_form_id(component, form):
    return '/'.join(value.replace('~','~0').replace('/','~1') for value in [component,form])
"#;

const TS_ENGLISH: &str = r#"const _englishSpace = "[\\u0009-\\u000d\\u0020\\u0085\\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000]";
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
    return /^[a-z][a-z'-]*$/.test(text.split(" ")[0]) ? text[0].toUpperCase() + text.slice(1) : text;
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
    if (options !== null && typeof options === "object" && !Array.isArray(options) && Object.keys(options).some(k => !["negation", "condition_order", "rendering"].includes(k))) return _englishStyle(value, options, random);
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
    const arities: any = {express:[1,2], expression:[1,2], expression_candidates:[1,2], realize: [1, 2], vary: [1, 3], combine: [2, 3], refer: [2, 3], remember: [2, 3], unmentioned: [2, 2], format_number: [1, 2], format_date: [1, 2], format_time: [1, 2]};
    if (!Object.hasOwn(arities, operation) || !Array.isArray(args) || args.length < arities[operation][0] || args.length > arities[operation][1]) throw new Error("invalid English call");
    if (["express","expression","expression_candidates"].includes(operation)) return _englishExpression(operation,args[0],args.length > 1 ? args[1] : {},random);
    if (operation === "realize") return _englishRender(_englishSentence(args[0]), _englishRenderOptions(args.length > 1 ? args[1] : {}));
    if (operation === "vary") {
        if (args.length > 2) {
            const draws = _englishArray(args[2]);
            let at = 0;
            random = () => at < draws.length ? draws[at++] : 0.5;
        }
        return _englishVary(args[0], args.length > 1 ? args[1] : {}, random);
    }
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

function _englishAsciiLower(text: string): string { return text.replace(/[A-Z]/g, c => c.toLowerCase()); }
function _englishStyleOptions(value: any): any {
    const data = _englishData.styles;
    const options = _englishObject(value, Object.keys(data.defaults));
    if (options.style != null) _englishEnum(options.style, Object.keys(data.presets));
    const result = {...(options.style != null ? data.presets[options.style] : data.defaults)};
    const optional = ['style','negation','condition_order','contractions','shorthand','casual','slang','reductions','fragments','warmth','soften_requests','punctuation','casing'];
    for (const [key, value] of Object.entries(options)) if (value !== null || !optional.includes(key)) result[key] = value;
    for (const key of optional.slice(1,-2)) if (typeof result[key] !== 'boolean') throw new Error('style controls must be booleans');
    _englishEnum(result.punctuation, ['preserve','omit_period','exclamation','expressive']);
    _englishEnum(result.casing, ['preserve','lowercase']);
    result.rendering = _englishRenderOptions(result.rendering);
    result.typos = _englishObject(result.typos, ['probability','max_errors','kinds'], data.defaults.typos);
    for (const value of [result.probability,result.typos.probability]) if (typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value > 1) throw new Error('invalid style probability');
    for (const value of [result.max_changes,result.typos.max_errors]) if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > 16) throw new Error('invalid style budget');
    for (const kind of _englishArray(result.typos.kinds)) _englishEnum(kind, ['common','omission','transposition','repetition']);
    result.protected = [..._englishArray(result.protected)];
    if (result.protected.some((term: any) => !_englishString(term))) throw new Error('empty protected term');
    result.alternatives = _englishArray(result.alternatives).map((family: any) => {
        family = _englishObject(family, ['from','to']);
        if (!_englishTrim(_englishString(family.from)) || !_englishArray(family.to).length || family.to.some((s: any) => !_englishTrim(_englishString(s)))) throw new Error('empty phrase alternatives');
        return family;
    });
    return result;
}
function _englishStyleProtected(text: string, terms: string[]): [number,number][] {
    const ranges: [number,number][] = [];
    for (const term of terms) {
        let start = text.indexOf(term);
        while (start >= 0) { ranges.push([start,start+term.length]); start = text.indexOf(term,start+term.length); }
    }
    let quoted: [number,string] | null = null;
    for (let i=0; i<text.length;) {
        const char = String.fromCodePoint(text.codePointAt(i)!);
        if (quoted !== null) { if (char === quoted[1]) { ranges.push([quoted[0],i+char.length]); quoted = null; } }
        else if (['"','`','“','‘'].includes(char) || char === "'" && (i === 0 || !/[\p{L}\p{N}]/u.test([...text.slice(0,i)].at(-1)!))) {
            quoted = [i,char === '“' ? '”' : char === '‘' ? '’' : char];
        }
        i += char.length;
    }
    if (quoted !== null) ranges.push([quoted[0],text.length]);
    for (const match of text.matchAll(new RegExp('[^'+_englishSpace.slice(1,-1)+']+', 'gu'))) {
        const core = match[0].replace(/[.,!?;)\]]+$/u, '');
        if (/[\p{N}@/\\_\-:.={}]/u.test(core)) ranges.push([match.index!,match.index!+match[0].length]);
    }
    return ranges;
}
function _englishStyleCase(source: string, replacement: string): string {
    return source && /\p{Lu}/u.test([...source][0]) && replacement && replacement.charCodeAt(0) < 128 ? replacement[0].toUpperCase()+replacement.slice(1) : replacement;
}
function _englishStyleBoundary(text: string, start: number, end: number): boolean {
    const word = (c: string) => c.codePointAt(0)! > 127 || /[\p{L}\p{N}_'’\-]/u.test(c);
    return (start === 0 || !word([...text.slice(0,start)].at(-1)!)) && (end === text.length || !word([...text.slice(end)][0]));
}
function _englishStyleStart(text: string, start: number): boolean {
    const before = _englishTrimEnd(text.slice(0,start));
    return !before || '.!?\n'.includes(before.at(-1)!);
}
function _englishStyleCandidates(text: string, options: any): any[] {
    const data = _englishData.styles, protectedRanges = _englishStyleProtected(text, options.protected), edits: any[] = [];
    const push = (rule: string, start: number, end: number, replacement: string, stage: number) => {
        if (text.slice(start,end) !== replacement && !protectedRanges.some(([a,b]) => start < b && a < end)) {
            const edit = {rule,start,end,replacement,stage};
            if (!edits.some(e => e.rule === rule && e.start === start && e.end === end && e.replacement === replacement && e.stage === stage)) edits.push(edit);
        }
    };
    const lower = _englishAsciiLower(text);
    for (const rule of data.rules) {
        if (!options[rule.category]) continue;
        let cursor = 0;
        while (cursor <= text.length) {
            const start = lower.indexOf(_englishAsciiLower(rule.from),cursor);
            if (start < 0) break;
            let stop = start+rule.from.length;
            cursor = stop;
            const source = text.slice(start,stop);
            if (![rule.from,_englishStyleCase('A',rule.from),_englishAsciiLower(rule.from)].includes(source)) continue;
            const initial = _englishStyleStart(text,start);
            if (!rule.from.endsWith(' ') && !_englishStyleBoundary(text,start,stop) || rule.from.endsWith(' ') && !initial) continue;
            const tail = text.slice(stop), found = tail.search(/[.?!\n]/), end = found < 0 ? tail.length : found, body = tail.slice(0,end);
            const words: string[] = body.match(new RegExp('[^'+_englishSpace.slice(1,-1)+']+', 'gu')) ?? [];
            const full = !_englishTrim(tail) || /^[.!?]/.test(tail), guard = rule.guard;
            let valid = false;
            if (guard === 'phrase') valid = true;
            else if (guard === 'initial') valid = initial;
            else if (guard === 'opening') valid = initial && (full || tail.startsWith(','));
            else if (guard === 'full') valid = initial && full;
            else if (guard === 'base') valid = initial && _englishPredicate('will',words) && !words.some(w => ['if','whether'].includes(w)) && (!rule.from.endsWith('going to') || !['work','school','college','church','bed','sleep','shop','practice'].includes(words[0]));
            else if (guard === 'progressive') valid = initial && _englishPredicate('am',words) && tail[end] !== '?' && /^[a-z]/.test(tail) && words.length > 0 && Object.values(_englishData.verbs).some((v: any) => words[0] === v.present_participle) && !words.some(w => ['not','if','and','but','or','because'].includes(w));
            else if (guard === 'request') valid = initial && words.length > 0 && tail[end] === '.' && !/[,;:]/.test(body) && !words.some(w => ['if','and','or','not'].includes(w));
            else if (guard.startsWith('aux:')) valid = initial && _englishPredicate(guard.slice(4),words);
            if (!valid) continue;
            let replacement = _englishStyleCase(source,rule.to);
            if (guard === 'progressive') { replacement = text[stop].toUpperCase(); stop++; }
            if (guard === 'request') { replacement += body+'?'; stop += end+1; }
            push(rule.id,start,stop,replacement,rule.stage);
        }
    }
    if (options.negation) {
        for (const match of text.matchAll(/[^.?!\n]*[.?!\n]|[^.?!\n]+$/g)) {
            const analysis = _englishAnalyze(match[0]);
            if (analysis?.edit) {
                const [a,b,replacement] = analysis.edit;
                if (replacement.includes("'")) push('negation:contract',match.index!+a,match.index!+b,replacement,0);
            }
        }
    }
    if (options.condition_order) for (const alternative of _englishTextAlternatives(text,{negation:false,condition_order:true})) push('condition:order',0,text.length,alternative,0);
    options.alternatives.forEach((family: any,index: number) => {
        let start = text.indexOf(family.from);
        while (start >= 0) {
            const end = start+family.from.length;
            if (_englishStyleBoundary(text,start,end)) family.to.forEach((replacement: string,choice: number) => push(`authored:${index}:${choice}`,start,end,replacement,1));
            start = text.indexOf(family.from,end);
        }
    });
    const end = _englishTrimEnd(text).length;
    if (end) {
        const mark = text[end-1], before = text.slice(0,end-1), words = before.match(new RegExp('[^'+_englishSpace.slice(1,-1)+']+', 'gu')) ?? [], last = words.at(-1) ?? '';
        const terminal = mark === '.' && !!last && !last.includes('.') && !data.abbreviations.includes(_englishAsciiLower(last));
        if (options.punctuation === 'omit_period' && terminal) push('punctuation:omit_period',end-1,end,'',3);
        else if (options.punctuation === 'exclamation' && terminal) push('punctuation:exclamation',end-1,end,'!',3);
        else if (options.punctuation === 'expressive' && (terminal || mark === '!' && !before.endsWith('!'))) push('punctuation:expressive',end-1,end,'!!',3);
    }
    if (options.casing === 'lowercase') for (const pronoun of data.contractions) {
        let start = text.indexOf(pronoun);
        while (start >= 0) {
            const end = start+pronoun.length;
            if (_englishStyleBoundary(text,start,end)) push('casing:lowercase',start,end,_englishAsciiLower(pronoun),3);
            start = text.indexOf(pronoun,end);
        }
    }
    for (const match of text.matchAll(/[A-Za-z]+/g)) {
        const start = match.index!, end = start+match[0].length, word = match[0], lower = _englishAsciiLower(word);
        if (!(data.words.includes(lower) || data.case_words.includes(lower)) || !_englishStyleBoundary(text,start,end)) continue;
        if (options.casing === 'lowercase') push('casing:lowercase',start,end,lower,3);
        if (!data.words.includes(lower) || word === 'could' || word.length < 5 || word !== lower || options.typos.probability === 0) continue;
        for (const kind of options.typos.kinds) {
            let replacement: string | undefined;
            if (kind === 'common') replacement = data.typos.find(([a,b]: string[]) => a === word)?.[1];
            else if (kind === 'omission') replacement = word.slice(0,2)+word.slice(3);
            else if (kind === 'transposition' && word[1] !== word[2]) replacement = word[0]+word[2]+word[1]+word.slice(3);
            else if (kind === 'repetition') replacement = word.slice(0,2)+word[1]+word.slice(2);
            if (replacement !== undefined && !data.words.includes(replacement) && !Object.values(_englishData.verbs).some((v: any) => Object.values(v).includes(replacement))) push('typo:'+kind,start,end,replacement,4);
        }
    }
    const compare = (a: string,b: string) => a < b ? -1 : a > b ? 1 : 0;
    return edits.sort((a,b) => a.stage-b.stage || a.start-b.start || compare(a.rule,b.rule) || compare(a.replacement,b.replacement));
}
function _englishStyle(value: any, supplied: any, random: () => number, details = false): any {
    const options = _englishStyleOptions(supplied);
    let text: string;
    if (typeof value === 'string') text = value;
    else {
        const sentence = _englishSentence(value);
        text = _englishRender(sentence,options.rendering);
        for (const clause of [sentence.clause,...(sentence.condition != null ? [sentence.condition] : [])]) {
            const subject = clause.subject.text;
            if (!['i','you','he','she','it','we','they'].includes(_englishAsciiLower(subject))) options.protected.push(subject,_englishStyleCase('A',subject));
        }
    }
    const edits: any[] = [];
    let budget = options.max_changes, gate: boolean | null = null;
    for (let stage=0; stage<=4; stage++) {
        if (!budget) break;
        let candidates = _englishStyleCandidates(text,options).filter(e => e.stage === stage);
        if (!candidates.length) continue;
        let limit = budget;
        if (stage < 4) {
            if (gate === null) gate = options.probability === 1 || options.probability > 0 && random() < options.probability;
            if (!gate) continue;
        } else {
            limit = Math.min(budget,options.typos.max_errors);
            const probability = options.typos.probability;
            if (!limit || probability === 0 || probability < 1 && random() >= probability) continue;
        }
        for (let i=0; i<limit; i++) {
            if (!candidates.length) break;
            const index = candidates.length === 1 ? 0 : Math.min(candidates.length-1,Math.floor(random()*candidates.length));
            const edit = candidates.splice(index,1)[0], {start,end,replacement} = edit;
            candidates = candidates.filter(e => !(start < e.end && e.start < end));
            const difference = replacement.length-(end-start);
            for (const other of candidates) if (other.start >= end) {other.start += difference; other.end += difference;}
            edits.push({rule:edit.rule,range:{start:new TextEncoder().encode(text.slice(0,start)).length,end:new TextEncoder().encode(text.slice(0,end)).length},replacement,stage:edit.stage});
            text = text.slice(0,start)+replacement+text.slice(end);
            budget--;
        }
    }
    return details ? {text,edits} : text;
}

function _englishExpressionIds(values: any): string[] {
    values = _englishArray(values);
    for (const value of values) _englishText(value);
    if (new Set(values).size !== values.length) throw new Error('duplicate expression identifiers');
    return values;
}
function _englishExpressionWeight(value: any, maximum = 1000000): number {
    if (typeof value !== 'number' || !Number.isFinite(value) || value < 0 || value > maximum) throw new Error('invalid expression weight or probability');
    return value;
}
function _englishExpressionInput(source: any, options: any): [any,any] {
    source = _englishObject(source,['intent','families','constructions','facts','optional_details','protected','topic','components'],{constructions:[],facts:[],optional_details:[],protected:[],topic:null,components:[]});
    _englishText(source.intent);
    source.constructions = _englishArray(source.constructions).map(c => _englishObject(c,['id','predicate','weight','requires','excludes','mentions'],{weight:1,requires:[],excludes:[],mentions:[]}));
    for (const c of source.constructions) {
        _englishText(c.id); _englishExpressionWeight(c.weight);
        c.predicate = _englishObject(c.predicate,['verb','complement'],{complement:null});
        c.predicate.verb = _englishVerb(c.predicate.verb);
        if (c.predicate.complement !== null) _englishFragment(c.predicate.complement);
        for (const key of ['requires','excludes','mentions']) _englishExpressionIds(c[key]);
        if (c.requires.some((id: string) => c.excludes.includes(id))) throw new Error('conflicting construction conditions');
    }
    const ids = _englishExpressionIds(source.constructions.map((c: any) => c.id));
    source.facts = _englishArray(source.facts).map(f => _englishObject(f,['id','text']));
    for (const f of source.facts) {_englishText(f.id); _englishText(f.text);}
    const facts = _englishExpressionIds(source.facts.map((f: any) => f.id));
    if (_englishExpressionIds(source.optional_details).some(id => !facts.includes(id))) throw new Error('unknown optional detail');
    if (_englishArray(source.protected).some(s => !_englishString(s))) throw new Error('empty protected term');
    const fields = ['id','kind','weight','constructions','patterns','requires','excludes','context_requires','establishes','mentions','reason','text','message'];
    const familyDefaults = {weight:1,constructions:[],patterns:[],requires:[],excludes:[],context_requires:[],establishes:[],mentions:[],reason:null,text:null,message:null};
    source.families = _englishArray(source.families).map(f => _englishObject(f,fields,familyDefaults));
    if (!source.families.length) throw new Error('no expression families');
    _englishExpressionIds(source.families.map((f: any) => f.id));
    for (const f of source.families) {
        _englishEnum(f.kind,Object.keys(_englishData.expressions.patterns)); _englishExpressionWeight(f.weight);
        for (const key of ['constructions','patterns','requires','excludes','context_requires','establishes','mentions']) _englishExpressionIds(f[key]);
        if (f.constructions.some((id: string) => !ids.includes(id)) || f.requires.some((id: string) => f.excludes.includes(id))) throw new Error('invalid family conditions');
        if (f.kind === 'structured') {
            if (f.message === null || f.text !== null || f.reason !== null || f.constructions.length || f.patterns.length) throw new Error('invalid structured family');
        } else if (['authored','follow_up'].includes(f.kind)) {
            _englishText(f.text);
            if (f.reason !== null || f.constructions.length || f.patterns.length || f.kind === 'follow_up' && !f.context_requires.length) throw new Error('invalid authored family');
        } else {
            if (f.text !== null || (f.kind === 'reason_request') !== (f.reason !== null)) throw new Error('invalid family text or reason');
            if (f.reason !== null) _englishText(f.reason);
            if (f.patterns.some((id: string) => !_englishData.expressions.patterns[f.kind].some((p: any) => p.id === id))) throw new Error('unknown expression pattern');
        }
    }
    const defaults = _englishData.expressions.options;
    options = _englishObject(options,['speaker','context','repeat_penalty'],defaults);
    const speaker = _englishObject(options.speaker,Object.keys(defaults.speaker),defaults.speaker);
    speaker.wording = _englishStyleOptions(speaker.wording);
    for (const key of ['family_weights','pattern_weights']) {
        if (speaker[key] === null || typeof speaker[key] !== 'object' || Array.isArray(speaker[key])) throw new Error('expected expression weights');
        for (const [name,weight] of Object.entries(speaker[key])) {_englishText(name); _englishExpressionWeight(weight);}
    }
    for (const key of ['greetings','closings']) {
        speaker[key] = _englishArray(speaker[key]).map(p => _englishObject(p,['text','weight'],{weight:1}));
        for (const p of speaker[key]) {_englishText(p.text); _englishExpressionWeight(p.weight);}
    }
    for (const key of ['greeting_probability','closing_probability','detail_probability']) _englishExpressionWeight(speaker[key],1);
    if (typeof speaker.max_details !== 'number' || !Number.isInteger(speaker.max_details) || speaker.max_details < 0 || speaker.max_details > 16) throw new Error('invalid detail budget');
    options.speaker = speaker;
    _englishExpressionWeight(options.repeat_penalty,1);
    const context = _englishObject(options.context,['established','recent','topic','known','requested','offered','allowed_acts'],{established:[],recent:[],topic:null,known:[],requested:[],offered:[],allowed_acts:[]});
    _englishExpressionIds(context.established);
    context.recent = _englishArray(context.recent).map(r => _englishObject(r,['intent','family','construction','pattern','message'],{construction:null}));
    if (context.recent.length > 32) throw new Error('history exceeds 32 selections');
    for (const r of context.recent) {
        for (const key of ['intent','family','pattern']) _englishText(r[key]);
        if (r.construction !== null) _englishText(r.construction);
    }
    options.context = context;
    _englishMessageInput(source,options);
    return [source,options];
}
function _englishExpressionComplete(text: string): string {return /[.?!]$/.test(text) ? text : text+'.';}
function _englishExpressionPreference(weights: any, key: string): number {return Object.hasOwn(weights,key) ? weights[key] : 1;}
function _englishExpressionCandidates(source: any, options: any): any[] {
    const facts = new Map<string,string>(source.facts.map((f: any) => [f.id,f.text]));
    const applies = (v: any) => [...v.requires,...v.mentions].every(id => facts.has(id)) && v.excludes.every((id: string) => !facts.has(id));
    const recent = options.context.recent.filter((r: any) => r.intent === source.intent), speaker = options.speaker;
    const result: any[] = [];
    let familyTotal = 0;
    for (const f of source.families) {
        if (!applies(f) || !f.context_requires.every((id: string) => options.context.established.includes(id)) || f.reason !== null && !facts.has(f.reason)) continue;
        let fw = f.weight*_englishExpressionPreference(speaker.family_weights,f.id)*(recent.some((r: any) => r.family === f.id) ? options.repeat_penalty : 1);
        if (f.message !== null) {
            const layout = _englishData.expressions.messages.layouts[f.message.layout];
            fw *= _englishExpressionPreference(speaker.organization,layout.organization)*_englishExpressionPreference(speaker.elaboration,_englishMessageElaboration(f.message,layout));
        }
        if (!fw) continue;
        const used = [...new Set([...f.mentions,...(f.reason !== null ? [f.reason] : [])])];
        let group: any[] = [];
        let total = 0;
        const candidate = (construction: string | null,pattern: string,text: string,mentions: any[],weight: number) => ({selection:{intent:source.intent,family:f.id,construction,pattern},text,used_facts:mentions,establishes:[...f.establishes],probability:weight});
        if (['authored','follow_up','structured'].includes(f.kind)) {
            if (_englishExpressionPreference(speaker.pattern_weights,f.kind) > 0) {total=1; group.push(candidate(null,f.kind,f.text ?? '',used,1));}
        } else {
            const patterns: [any,number][] = _englishData.expressions.patterns[f.kind].filter((p: any) => (!f.patterns.length || f.patterns.includes(p.id)) && _englishExpressionPreference(speaker.pattern_weights,p.id) > 0).map((p: any) => [p,_englishExpressionPreference(speaker.pattern_weights,p.id)]);
            const patternTotal = patterns.reduce((n,[p,w]) => n+w,0);
            if (!patternTotal) continue;
            for (const c of source.constructions) {
                if (f.constructions.length && !f.constructions.includes(c.id) || !applies(c)) continue;
                const weight = c.weight*(recent.some((r: any) => r.construction === c.id) ? options.repeat_penalty : 1);
                if (!weight) continue;
                total += weight;
                const mentions = [...new Set([...c.mentions,...used])];
                for (const [pattern,pw] of patterns) {
                    const predicate = c.predicate, action = predicate.verb[pattern.gerund ? 'present_participle' : 'base']+(predicate.complement !== null ? ' '+predicate.complement : '');
                    let text = pattern.prefix+action+pattern.suffix;
                    if (f.reason !== null) text = _englishExpressionComplete(facts.get(f.reason)!)+' '+text;
                    group.push(candidate(c.id,pattern.id,text,mentions,weight*pw/patternTotal));
                }
            }
        }
        if (f.message !== null) {group=_englishMessageExpand(source,f,options,group); total=group.reduce((n,c)=>n+c.probability,0);}
        if (total > 0) {familyTotal+=fw; for (const c of group) c.probability=c.probability/total*fw; result.push(...group);}
    }
    for (const c of result) c.probability/=familyTotal;
    return result;
}
function _englishExpressionPick(weights: number[], random: () => number): number {
    if (weights.length === 1) return 0;
    let draw=random()*weights.reduce((n,w)=>n+w,0);
    for (let i=0;i<weights.length;i++) {if (draw < weights[i]) return i; draw-=weights[i];}
    return weights.length-1;
}
function _englishExpression(operation: string, supplied: any, configuration: any, random: () => number): any {
    const [source,options] = _englishExpressionInput(supplied,configuration), candidates = _englishExpressionCandidates(source,options);
    if (operation === 'expression_candidates') return candidates;
    if (!candidates.length) throw new Error('no eligible expression with positive weight');
    const chosen=candidates[_englishExpressionPick(candidates.map(c=>c.probability),random)], speaker=options.speaker, context=options.context;
    const chance=(p: number)=>p===1 || p>0 && random()<p;
    const phrase=(key: string,p: number): string | null => {const values=speaker[key].filter((v: any)=>v.weight>0); return values.length && chance(p) ? values[_englishExpressionPick(values.map((v: any)=>v.weight),random)].text : null;};
    const parts: string[] = [], greeting=context.recent.length ? null : phrase('greetings',speaker.greeting_probability);
    if (greeting !== null) parts.push(greeting);
    parts.push(chosen.text);
    const used=[...chosen.used_facts], details=source.optional_details.filter((id: string)=>!used.includes(id) && !context.established.includes(id));
    for (let i=0;i<speaker.max_details;i++) {
        if (!details.length || !chance(speaker.detail_probability)) break;
        const id=details.splice(_englishExpressionPick(details.map(()=>1),random),1)[0];
        parts.push(_englishExpressionComplete(source.facts.find((f: any)=>f.id===id).text)); used.push(id);
    }
    const closing=phrase('closings',speaker.closing_probability);
    if (closing !== null) parts.push(closing);
    const base=parts.join(' '), wording={...speaker.wording,protected:[...speaker.wording.protected,...source.protected]};
    if (chosen.selection.message) wording.fragments=false;
    const styled=_englishStyle(base,wording,random,true);
    const result={text:styled.text,base_text:base,selection:chosen.selection,used_facts:used,edits:styled.edits,context:{established:[...new Set([...context.established,...chosen.establishes,...used])],recent:[...context.recent,chosen.selection].slice(-32)}};
    result.context=_englishMessageContext(source,chosen,used,context,result.context);
    return operation === 'express' ? result.text : result;
}
function _englishMessageElaboration(plan: any, layout: any): string {
    return plan.details.length ? 'detailed' : layout.act !== null || plan.layout === 'request_only' ? 'minimal' : 'relevant_context';
}
function _englishMessageInput(source: any, options: any): void {
    const data=_englishData.expressions.messages, context=options.context;
    for (const topic of [source.topic,context.topic]) if (topic !== null) _englishText(topic);
    for (const key of ['known','requested','offered','allowed_acts']) _englishExpressionIds(context[key]);
    for (const act of context.allowed_acts) _englishEnum(act,['answer','correction','acknowledgment','clarification','follow_up_request']);
    for (const key of Object.keys(data.preferences)) {
        const weights=options.speaker[key];
        if (weights === null || typeof weights !== 'object' || Array.isArray(weights)) throw new Error('invalid message preferences');
        for (const [name,weight] of Object.entries(weights)) {_englishEnum(name,data.preferences[key]); _englishExpressionWeight(weight);}
    }
    source.components=_englishArray(source.components).map(c=>_englishObject(c,['id','role','forms','mentions','requires','context_requires','slot','option'],{mentions:[],requires:[],context_requires:[],slot:null,option:null}));
    _englishExpressionIds(source.components.map((c: any)=>c.id));
    for (const c of source.components) {
        _englishEnum(c.role,data.roles);
        for (const key of ['mentions','requires','context_requires']) _englishExpressionIds(c[key]);
        for (const key of ['slot','option']) if (c[key] !== null) _englishText(c[key]);
        c.forms=_englishArray(c.forms).map(f=>_englishObject(f,['id','text','shape','reference','weight','context_requires'],{shape:'complete',reference:'explicit',weight:1,context_requires:[]}));
        if (!c.forms.length) throw new Error('empty component forms');
        _englishExpressionIds(c.forms.map((f: any)=>f.id));
        for (const f of c.forms) {
            _englishText(f.text); _englishExpressionWeight(f.weight); _englishExpressionIds(f.context_requires);
            _englishEnum(f.shape,['complete','fragments','clause','value']); _englishEnum(f.reference,['explicit','contextual']);
            if (f.reference === 'contextual' && (source.topic === null || !c.context_requires.length && !f.context_requires.length) || f.shape !== 'complete' && /[.?!]$/.test(f.text)) throw new Error('invalid component grammar or reference guard');
        }
    }
    const components=new Map<string,any>(source.components.map((c: any)=>[c.id,c]));
    for (const f of source.families) {
        if (f.message === null) continue;
        const p=_englishObject(f.message,['layout','bindings','relation','allow_apology','details'],{bindings:{},relation:null,allow_apology:false,details:[]});
        f.message=p;
        _englishEnum(p.layout,Object.keys(data.layouts));
        const layout=data.layouts[p.layout];
        if (typeof p.allow_apology !== 'boolean') throw new Error('invalid apology flag');
        _englishExpressionIds(p.details);
        if (p.bindings === null || typeof p.bindings !== 'object' || Array.isArray(p.bindings) || Object.keys(p.bindings).length !== layout.roles.length || !layout.roles.every((r: string)=>Object.hasOwn(p.bindings,r)) || (f.kind === 'structured') !== layout.standalone || f.kind === 'reason_request' || source.optional_details.length || p.allow_apology && p.layout !== 'correction') throw new Error('incompatible message layout');
        if (p.relation !== null && !(['request_reason','reason_request'].includes(p.layout) && p.relation === 'cause' || p.layout === 'current_desired' && p.relation === 'contrast')) throw new Error('invalid message relationship');
        for (const [role,id] of Object.entries(p.bindings)) {
            _englishText(id);
            if (!components.has(id as string) || components.get(id as string).role !== role) throw new Error('invalid message binding');
        }
        for (const id of p.details) if (!components.has(id) || components.get(id).role !== 'detail') throw new Error('invalid message detail');
        if (layout.act !== null && source.topic === null) throw new Error('responses require a topic');
        if (['answer','clarification','correction'].includes(p.layout)) {
            const bound=Object.values(p.bindings).map(id=>components.get(id as string));
            if (bound.some(c=>c.slot === null) || new Set(bound.map(c=>c.slot)).size !== 1 || p.layout === 'correction' && (bound.some(c=>!c.mentions.length) || bound[0].mentions.some((id: string)=>bound[1].mentions.includes(id)))) throw new Error('invalid response slots or facts');
        }
    }
    for (const r of context.recent) {
        if (r.message == null) delete r.message;
        if (r.message != null) {
        const m=_englishObject(r.message,['layout','recipe','forms','organization','elaboration','sentence_shape','reference_style']);
        for (const key of ['layout','recipe','organization','elaboration','sentence_shape','reference_style']) _englishText(m[key]);
        _englishExpressionIds(m.forms);
        }
    }
}
function _englishMessageExpand(source: any, family: any, options: any, bases: any[]): any[] {
    const data=_englishData.expressions.messages, plan=family.message, context=options.context, speaker=options.speaker, layout=data.layouts[plan.layout];
    const topicMatches=source.topic !== null && source.topic === context.topic;
    if (layout.act !== null && (!topicMatches || !context.allowed_acts.includes(layout.act))) return [];
    const components=new Map<string,any>(source.components.map((c: any)=>[c.id,c]));
    for (const [role,id] of Object.entries(plan.bindings)) {
        const c=components.get(id as string);
        if (['answer','clarification'].includes(plan.layout) && !context.requested.includes(c.slot) || role === 'previous' && !c.mentions.every((i: string)=>context.established.includes(i))) return [];
    }
    const facts=[...source.facts.map((f: any)=>f.id),...context.known];
    const applies=(c: any)=>[...c.mentions,...c.requires].every(i=>facts.includes(i)) && c.context_requires.every((i: string)=>context.established.includes(i)) && (c.option === null || topicMatches && context.offered.includes(c.option));
    const organization=layout.organization, elaboration=_englishMessageElaboration(plan,layout), preference=_englishExpressionPreference(speaker.organization,organization)*_englishExpressionPreference(speaker.elaboration,elaboration);
    if (!preference) return [];
    const result: any[]=[];
    for (const base of bases) {
        const expanded: any[]=[];
        for (const recipe of data.recipes) {
            if (recipe.layout !== plan.layout || recipe.relation !== undefined && recipe.relation !== plan.relation || recipe.apology && !plan.allow_apology || recipe.declarative && !['desire_would_like','desire_want','desire_looking_to','need_need_to'].includes(base.selection.pattern)) continue;
            let choices: [string[],string[],boolean,number][]=[[[...base.used_facts],[],recipe.reference === 'contextual',1]];
            const slots: [string,any,string][]=[...layout.roles.map((role: string)=>[role,components.get(plan.bindings[role]),recipe.forms[role]]),...plan.details.map((id: string)=>['detail',components.get(id),'complete'])];
            for (const [role,c,shape] of slots) {
                if (!applies(c)) {choices=[]; break;}
                const forms=c.forms.filter((f: any)=>f.shape === shape && f.weight > 0 && !(role === 'current' && recipe.relation === 'contrast' && /[?!]$/.test(f.text)) && f.context_requires.every((i: string)=>context.established.includes(i)) && (f.reference !== 'contextual' || topicMatches));
                const total=forms.reduce((n: number,f: any)=>n+f.weight,0);
                if (choices.length*forms.length > 16384) throw new Error('message expansion exceeds 16384 choices');
                const following: typeof choices=[];
                for (const [used,selected,contextual,w] of choices) for (const f of forms) {
                    following.push([[...new Set([...used,...c.mentions])],[...selected,_englishMessageFormId(c.id,f.id)],contextual || f.reference === 'contextual',w*f.weight/total]);
                }
                choices=following;
            }
            const shape=plan.details.length && recipe.shape === 'complete' ? 'short_sentences' : recipe.shape;
            for (const [used,forms,contextual,w] of choices) {
                const text=_englishMessageRender(recipe,plan,components,layout,base.text,forms);
                if (contextual && !topicMatches) continue;
                const reference=contextual ? 'contextual' : 'explicit', probability=w*_englishExpressionPreference(speaker.sentence_shape,shape)*_englishExpressionPreference(speaker.reference_style,reference);
                if (!probability) continue;
                const selection={...base.selection,message:{layout:plan.layout,recipe:recipe.id,forms,organization,elaboration,sentence_shape:shape,reference_style:reference}};
                expanded.push({...base,text,used_facts:used,selection,probability});
            }
        }
        const total=expanded.reduce((n,c)=>n+c.probability,0);
        for (const c of expanded) c.probability=c.probability/total*base.probability*preference;
        result.push(...expanded);
        if (result.length > 16384) throw new Error('message expansion exceeds 16384 choices');
    }
    return result;
}
function _englishMessageContext(source: any, chosen: any, used: string[], previous: any, context: any): any {
    for (const key of ['topic','known','requested','offered','allowed_acts']) context[key]=key === 'topic' ? previous[key] : [...previous[key]];
    if (source.topic !== null) {
        if (context.topic !== source.topic) for (const key of ['requested','offered','allowed_acts']) context[key]=[];
        context.topic=source.topic;
        context.known=[...new Set([...context.known,...used])];
    }
    if (chosen.selection.message) {
        const plan=source.families.find((f: any)=>f.id === chosen.selection.family).message;
        const component=(role: string)=>source.components.find((c: any)=>c.id === plan.bindings[role]);
        if (plan.layout === 'answer') context.requested=context.requested.filter((i: string)=>i !== component('answer').slot);
        if (plan.layout === 'correction') context.known=context.known.filter((i: string)=>!component('previous').mentions.includes(i));
        if (_englishData.expressions.messages.layouts[plan.layout].act !== null) context.allowed_acts=[];
    }
    return Object.fromEntries(Object.entries(context).filter(([k,v])=>['established','recent'].includes(k) || v !== null && !(Array.isArray(v) && !v.length)));
}

function _englishMessageRender(recipe: any, plan: any, components: Map<string,any>, layout: any, request: string, forms: string[]): string {
    const values=new Map<string,string>([['request',request],['request_clause',request.replace(/\.+$/,'')]]), details: string[]=[];
    const bindings: [string,string][]=[...layout.roles.map((r: string)=>[r,plan.bindings[r]]),...plan.details.map((id: string)=>['detail',id])];
    for (let i=0;i<bindings.length;i++) {
        const [role,id]=bindings[i], form=components.get(id).forms.find((f: any)=>_englishMessageFormId(id,f.id) === forms[i]);
        const value=form.shape === 'complete' ? _englishExpressionComplete(form.text) : form.text;
        if (role === 'detail') details.push(value);
        else {values.set(role,value); values.set(role+'_clause',value.replace(/\.+$/,''));}
    }
    const rendered=recipe.text.replace(/\{([^}]+)\}/g,(_: string,key: string)=>values.get(key)!);
    return [rendered,...details].join(' ');
}

function _englishMessageFormId(component: string, form: string): string {
    return [component,form].map(value=>value.replaceAll('~','~0').replaceAll('/','~1')).join('/');
}
"#;

/// The runtime text for one language.
///
/// It stands alone, so a scorer file can carry it. An agent's runtime module also
/// gets `_english`, which hands the agent's values and random source to it.
pub(crate) fn source(lang: ScorerLang, agent: bool) -> String {
    let data = super::english::runtime_data().to_string();
    match lang {
        ScorerLang::Python => format!(
            "import json as _english_json\nimport re as _english_re\nimport math as _english_math\n\nclass _EnglishData:\n    data = _english_json.loads({})\n\n{}\n{}",
            serde_json::to_string(&data).unwrap(),
            PY_ENGLISH,
            if agent {
                "def _english(operation, args, random_source):\n    return _language_english(operation, _value(args), lambda: _draw(random_source))\n"
            } else {
                ""
            }
        ),
        ScorerLang::Typescript => format!(
            "const _englishData: any = {data};\n{}\n{}",
            TS_ENGLISH,
            if agent {
                "function _english(operation: string, args: unknown[], randomSource: any): any { return _language_english(operation, _serialize(args), () => _draw(randomSource)); }\n"
            } else {
                ""
            }
        ),
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::mocking::data::{self, Distribution};
    use crate::providers::{Provider, With, braintrust, world};
    use serde_json::{Value, json};
    use std::{
        io::Write,
        process::{Command, Stdio},
        time::{Duration, UNIX_EPOCH},
    };

    fn literal(value: &Value) -> String {
        match value {
            Value::Object(fields) => format!(
                "{{{}}}",
                fields
                    .iter()
                    .map(|(key, value)| format!("{key}: {}", literal(value)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Array(items) => format!("[|{}|]", items.iter().map(literal).collect::<Vec<_>>().join(", ")),
            value => value.to_string(),
        }
    }

    fn native(operation: &str, args: &[Value]) -> Value {
        let source = format!(
            "(language_root:task {{name: \"language\"}}) {{output: {operation} ({}), input: language.input}};\n(language:agent) {{input: null, output: language_root.output}};\n(language)-[:next]->(language_root);",
            args.iter().map(literal).collect::<Vec<_>>().join(", ")
        );
        let model = With(braintrust::Bundle, world::Bundle)
            .compile(&source)
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        match data::generate(
            model,
            1,
            Duration::from_secs(1),
            Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(10),
            42,
            &(),
        ) {
            Ok(batch) => json!({"value":batch.events[0].output.as_ref().unwrap().project(&()).unwrap()}),
            Err(_) => json!({"error":true}),
        }
    }

    #[test]
    fn portable_english_matches_native_values_errors_and_text_alternatives() {
        let sentence = json!({"clause":{"subject":{"text":"constructor"},"predicate":{"verb":"write","complement":"letters"},"tense":"past","aspect":"perfect"}});
        let cause = json!({"clause":{"subject":{"text":"Maya"},"predicate":{"verb":"arrive"},"tense":"past"}});
        let entity = json!({"id":"maya","label":"Maya","pronouns":{"subject":"she","object":"her","possessive_determiner":"her","possessive":"hers"}});
        let context = json!({"entities":[entity.clone()],"mentioned":["maya"],"facts":["arrival"]});
        let competing = json!({"entities":[entity.clone(),{"id":"other","label":"Chris"}],"mentioned":["maya","other"]});
        let mut cases = vec![
            ("realize", vec![sentence.clone()]),
            ("combine", vec![json!([sentence.clone(), cause.clone()]), json!("because")]),
            ("refer", vec![entity.clone(), context.clone()]),
            ("refer", vec![entity.clone(), competing.clone()]),
            ("refer", vec![entity.clone(), competing, json!({"style":"pronoun"})]),
            ("remember", vec![json!({}), json!([entity.clone()]), json!(["arrival"])]),
            (
                "unmentioned",
                vec![
                    json!([{"id":"arrival","text":"late"},{"id":"booking","text":"ZX-42"}]),
                    context,
                ],
            ),
            (
                "realize",
                vec![json!({"clause":{"subject":{"text":"toString"},"predicate":{"verb":"constructor"}}})],
            ),
            ("realize", vec![sentence.clone(), json!({"negation":"informal"})]),
            (
                "vary",
                vec![
                    json!("Constructor is not ready."),
                    json!({"negation":false,"condition_order":false}),
                ],
            ),
            ("vary", vec![json!("The guest cannot leave."), json!({"negation":null})]),
        ];
        for value in ["2024-02-29", "1900-02-29", "0000-01-01", "2026-11-10\n"] {
            for locale in ["en-US", "en-GB", "en-CA", "en-AU", "en-IN", "fr-FR"] {
                cases.push(("format_date", vec![json!(value), json!({"locale":locale})]));
            }
        }
        for value in ["00:05:09", "23:59", "24:00", "12:00\n"] {
            cases.push(("format_time", vec![json!(value), json!({"locale":"en-AU","seconds":true})]));
        }
        for value in [
            "1234567.895",
            "-0.004",
            "9.995",
            "1e-8",
            "0e4096",
            "1e-4096",
            "1\n",
            ".5",
            "5.",
        ] {
            cases.push((
                "format_number",
                vec![json!(value), json!({"locale":"en-IN","fraction_digits":2})],
            ));
        }
        for precision in [json!(true), json!(1.0), json!(1.5), Value::Null, json!(10)] {
            cases.push(("format_number", vec![json!(2.675), json!({"fraction_digits":precision})]));
        }
        for tense in ["present", "past", "future"] {
            for aspect in ["simple", "progressive", "perfect", "perfect_progressive"] {
                for form in ["statement", "question"] {
                    let mut value = sentence.clone();
                    value["clause"]["tense"] = json!(tense);
                    value["clause"]["aspect"] = json!(aspect);
                    value["clause"]["form"] = json!(form);
                    value["clause"]["polarity"] = json!("negative");
                    cases.push(("realize", vec![value, json!({"negation":"contracted"})]));
                }
            }
        }
        let texts = [
            "I won't leave. They cannot stay.",
            "I cannot leave.They cannot stay.",
            "I cannot leave. they cannot stay.",
            "I cannot leave. api_v2 cannot stay.",
            "I cannot leave? They cannot stay.",
            "Can they leave?They cannot stay.",
            "Can they leave? they cannot stay.",
            "I cannot leave.\nthey cannot stay.",
            "I cannot leave.They cannot stay.",
            "I cannot leave. Καλλιόπη cannot stay.",
            "If the guest arrives late, can they check in?",
            "Can they check in if the guest arrives late?",
            "  If Maya has finished, api_v2 will update résumé_ID-42.\n",
            "Constructor is not ready.",
            "They cannot\u{001c}leave.",
            "\u{feff}They cannot leave.",
            "\u{0085}They cannot leave.\u{00a0}",
            "I can't leave. She's gone.",
        ];
        let expected = json!({"calls":cases.iter().map(|(op,args)|native(op,args)).collect::<Vec<_>>(),"texts":texts.iter().map(|text|super::super::english::text_alternatives(text,&super::super::english::VariationOptions{negation:true,condition_order:true})).collect::<Vec<_>>()});
        let input = json!({"calls":cases,"texts":texts}).to_string();
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            let trailer = if lang == ScorerLang::Python {
                r#"
data = _english_json.load(__import__('sys').stdin)
results = []
for operation, args in data['calls']:
    try: results.append({'value':_language_english(operation,args,lambda:0.0)})
    except Exception: results.append({'error':True})
print(_english_json.dumps({'calls':results,'texts':[_english_text_alternatives(text,{'negation':True,'condition_order':True}) for text in data['texts']]}))
"#
            } else {
                r#"
import {readFileSync} from 'node:fs';
const data = JSON.parse(readFileSync(0,'utf8'));
const calls = data.calls.map(([operation,args]: any) => {try {return {value:_language_english(operation,args,()=>0)};} catch {return {error:true};}});
console.log(JSON.stringify({calls,texts:data.texts.map((text:string)=>_englishTextAlternatives(text,{negation:true,condition_order:true}))}));
"#
            };
            let source = format!("{}\n{trailer}", source(lang, false));
            let path = std::env::temp_dir().join(format!(
                "bts-english-runtime-{}.{}",
                uuid::Uuid::new_v4(),
                if lang == ScorerLang::Python { "py" } else { "mts" }
            ));
            std::fs::write(&path, source).unwrap();
            let mut child = Command::new(if lang == ScorerLang::Python { "python3" } else { "node" })
                .arg(&path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            let output = child.wait_with_output().unwrap();
            std::fs::remove_file(path).unwrap();
            assert!(
                output.status.success(),
                "{lang:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
            for (index, case) in cases.iter().enumerate() {
                assert_eq!(actual["calls"][index], expected["calls"][index], "{lang:?}: {case:?}");
            }
            assert_eq!(actual["texts"], expected["texts"], "{lang:?} text alternatives");
        }
    }
}
