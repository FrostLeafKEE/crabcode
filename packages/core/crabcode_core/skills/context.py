"""Deduplicate auto-triggered skill content against the retained conversation."""

import hashlib

from crabcode_core.types.message import create_user_message


def auto_skill_messages(skills, messages):
    retained = {(message.origin, message.content) for message in messages if isinstance(message.content, str)}
    result = []
    for skill in skills:
        content = f"[Auto-triggered skill: {skill.name}] {skill.description or ''}\n{skill.content}"
        origin = "auto-skill:" + hashlib.sha256(content.encode()).hexdigest()
        wrapped = ("<system-reminder>\nFollow this skill when relevant to the user's request:\n"
                   + content + "\n</system-reminder>")
        if (origin, wrapped) in retained:
            continue
        retained.add((origin, wrapped))
        result.append(create_user_message(
            content=wrapped,
            origin=origin,
        ))
    return result
