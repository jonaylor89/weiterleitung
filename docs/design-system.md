# Design system

One stylesheet (`static/app.css`), one script (`static/app.js`), and a handful
of Askama macros under `templates/web/components/`. No build step, no npm, no
webfonts: both assets are `include_str!`-ed into the binary and served from
`GET /static/app.css` and `GET /static/app.js` with an ETag and
`Cache-Control: public, max-age=0, must-revalidate`, so a repeat visit costs a
304 and an upgraded binary never serves a stale stylesheet.

Pages are composed from the macros below. If a page needs styling that is not
here, add a component — do not write page-local CSS.

## Tokens

All tokens are CSS custom properties on `:root`, overridden wholesale inside a
`prefers-color-scheme: dark` block, so components never branch on theme.

| Group | Tokens |
| --- | --- |
| type | `--font-sans`, `--font-mono`, `--text-xs` .75rem, `--text-sm` .8125rem, `--text-md` .9375rem (body), `--text-lg` 1.0625rem, `--text-xl` 1.375rem |
| space | `--space-1` .25rem, `--space-2` .5rem, `--space-3` .75rem, `--space-4` 1.25rem, `--space-5` 2rem, `--space-6` 3rem |
| radii | `--radius-sm` .25rem, `--radius-md` .5rem (controls), `--radius-lg` .75rem (cards), `--radius-full` |
| shadow | `--shadow-sm` (cards), `--shadow-md` (overlays) |
| surface | `--bg` (page), `--surface` (cards, controls), `--surface-subtle` (table head, hovers), `--border`, `--border-strong` (control outlines) |
| text | `--fg`, `--fg-muted` |
| intent | `--accent` / `--accent-hover` / `--accent-fg` / `--accent-soft`, `--danger` / `--danger-soft`, `--success` / `--success-soft`, `--warning` / `--warning-soft`, `--focus` |

Six spacing steps and five type steps are deliberate: a dense single-user admin
tool does not need a twelve-step scale, and a small set keeps pages rhythmically
consistent.

## Layout primitives

```html
{% extends "web/_layout.html" %}
{% block title %}mailboxes &middot; weiterleitung{% endblock %}
{% block nav_mailboxes %}aria-current="page"{% endblock %}

{% block content %}
<div class="page__header">
  <h1 class="page__title">mailboxes</h1>
  <p class="page__subtitle">the real inboxes your aliases forward to</p>
</div>

<section class="section">
  <div class="section__header">
    <h2 class="section__title">new mailbox</h2>
    <p class="section__hint">optional one-liner</p>
  </div>
  ...
</section>
{% endblock %}
```

- `.shell` / `.topbar` / `.page` — the frame, supplied by `_layout.html`.
  Override `nav_aliases` or `nav_mailboxes` to mark the current nav item.
- `.section` stacks a `.section__header` and its content.
- `.card` + `.card__body` (`.card--flush` when a table fills it,
  `.card__footer` for a trailing caveat).
- `.stack` (vertical gap), `.row` (horizontal wrap), `.centered` (login).

## Components

Import what a page uses:

```html
{%- import "web/components/button.html" as button -%}
{%- import "web/components/feedback.html" as feedback -%}
{%- import "web/components/form.html" as form -%}
{%- import "web/components/copy.html" as copy -%}
```

### Buttons — `button.html`

`variant` is `"neutral"`, `"primary"` or `"danger"` (destructive actions are
always `"danger"`).

```html
{% call button::button("create", "primary", "submit") %}
{% call button::link_button("/admin/dashboard", "back", "neutral") %}
{% call button::post_button("/admin/aliases/{}/delete"|format(alias.alias_id), "delete", "danger") %}
```

`post_button` renders a whole single-button `<form method="post">`, which is how
every mutation in this app is triggered.

### Form fields — `form.html`

Every input gets a real `<label for>`; ids are prefixed `f-` from the field
name. Pass `""` for an unused placeholder or hint.

```html
<form class="card__body form-row" action="/admin/mailboxes" method="post">
  {% call form::field("email", "address", "email", "me@personal.example", "") %}
  {% call form::checkbox("is_default", "use as default", "true") %}
  <div class="form-row__action">
    {% call button::button("add", "primary", "submit") %}
  </div>
</form>
```

- `field(name, label, kind, placeholder, hint)` — labelled text input.
- `field_error(name, label, kind, error)` — the invalid state, wired with
  `aria-invalid` and `aria-describedby`.
- `auth_field(name, label, kind, autocomplete, autofocus)` — login inputs.
- `mailbox_select(name, label, mailboxes)` — `<select>` over `MailboxView`s.
- `checkbox(name, label, value)`.
- `.form-row` lays fields out inline and wraps on narrow screens;
  `.form-row__suffix` is for static text like `@example.com`.

### Tables — `.table` inside `.card--flush > .table-wrap`

```html
<table class="table">
  <thead>
    <tr><th scope="col">alias</th><th scope="col" class="table__num">fwd</th><th scope="col"></th></tr>
  </thead>
  <tbody>
    <tr>
      <td data-label="alias">{% call copy::copy(alias.address) %}</td>
      <td data-label="forwarded" class="table__num">{{ alias.forward_count }}</td>
      <td><div class="table__actions">...</div></td>
    </tr>
    {% if aliases.is_empty() %}
      {% call feedback::empty_row(3, "No aliases yet", "Create one above.") %}
    {% endif %}
  </tbody>
</table>
```

Every cell carries `data-label`. Below 40rem the header is hidden and each row
becomes a stacked card whose cells print their `data-label` as a leading
caption — no horizontal scrolling on a phone, no duplicated markup.

### Badges — `feedback.html`

`tone` is `"neutral"`, `"success"`, `"danger"`, `"warning"` or `"accent"`.

```html
{% call feedback::badge("enabled", "success") %}
{% call feedback::status_badge(message.status) %}   {# maps a delivery status onto a tone #}
```

### Alerts, stat tiles, empty states — `feedback.html`

```html
{% call feedback::alert("Created shop.7f3a@example.com", false) %}  {# true = error #}
<div class="stats">{% call feedback::stat("aliases", aliases.len()) %}</div>
{% call feedback::empty("No mailboxes yet", "Add the inbox you actually read.") %}
{% call feedback::empty_row(4, "No contacts yet", "They show up after the first mail.") %}
```

Flash messages are rendered by the layout; call `alert` directly only outside
it (the login page does).

### Copy to clipboard — `copy.html`

```html
{% call copy::copy(alias.address) %}
{% call copy::copy_link("/admin/aliases/{}"|format(alias.alias_id), alias.address) %}
```

The only JavaScript in the app is one delegated `click` listener in
`static/app.js` that reads `data-copy`. Without JS the address is still plain
selectable text.

## Utilities

`.muted`, `.mono`, `.row`, `.stack`, `.push-right`, `.visually-hidden`. That is
the whole escape hatch; anything else belongs in a component.

## Conventions

- Semantic elements first: `header`/`nav`/`main`/`section`, `th scope="col"`.
- Focus is never removed — `:focus-visible` draws a 2px `--focus` ring.
- Destructive actions use the `danger` variant, everywhere, and only there.
- Both themes are checked at `/admin/styleguide`, which renders every component.
