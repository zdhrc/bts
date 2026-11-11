_english_space = "\t\n\v\f\r \u0085\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000"
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
    words = ["if", "the", "a", "an", "this", "that", "these", "those", "my", "your", "our", "their", "we", "you", "he", "she", "it", "they", "can", "could", "may", "might", "must", "should", "will", "would", "am", "is", "are", "was", "were", "have", "has", "had", "do", "does", "did"] + [row[2] for row in _EnglishData.data["negations"]]
    return text[0].upper() + text[1:] if text.split(" ")[0] in words else text

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
    arities = {"realize": (1, 2), "vary": (1, 2), "combine": (2, 3), "refer": (2, 3), "remember": (2, 3), "unmentioned": (2, 2), "format_number": (1, 2), "format_date": (1, 2), "format_time": (1, 2)}
    if operation not in arities or type(args) is not list or not arities[operation][0] <= len(args) <= arities[operation][1]: raise ValueError("invalid English call")
    if operation == "realize": return _english_render(_english_sentence(args[0]), _english_render_options(args[1] if len(args) > 1 else {}))
    if operation == "vary": return _english_vary(args[0], args[1] if len(args) > 1 else {}, random)
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
