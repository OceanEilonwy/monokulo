// Renders each example in generated/fixtures.json through the generated
// Solid component, and compares the DOM with the HTML the Rust component
// rendered for the same props. solid-equivalence.spec.js reads the result.
import { createComponent } from 'solid-js';
import { render } from '@solidjs/web';
import type { JSX } from '@solidjs/web';
import * as components from '@monokulo/ui/solid';
import { islands } from '../../solid/islands';
import fixtures from './generated/fixtures.json';

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

const registry = components as unknown as Record<string, (props: Record<string, unknown>) => JSX.Element>;

/** Fixture JSON to props: `$slot` content becomes JSX, components and text. */
function props(value: Json): unknown {
  if (Array.isArray(value)) return value.map(props);
  if (value !== null && typeof value === 'object') {
    if ('$slot' in value) {
      return (value.$slot as Json[]).map(part => {
        if (typeof part === 'string') return part;
        const { $component, props: inner } = part as { $component: string; props: Json };
        return createComponent(registry[$component], props(inner) as Record<string, unknown>);
      });
    }
    return Object.fromEntries(Object.entries(value).map(([key, inner]) => [key, props(inner)]));
  }
  return value;
}

/** A DOM tree as comparable data: attributes sorted, text merged, empty
 *  text and comments dropped. Form controls compare their live value,
 *  since frameworks set it as a property rather than an attribute. The
 *  server's island wrapper, its props and its attach marks (`data-mk`) are
 *  for the behaviour, not the markup: they're left out. */
function canon(node: Node): unknown {
  if (node.nodeType === Node.TEXT_NODE) return node.textContent;
  const element = node as Element;
  const attrs = [...element.attributes]
    .filter(a => !(a.name === 'value' && element instanceof HTMLInputElement)
      && !(a.name === 'selected' && element instanceof HTMLOptionElement) && a.name !== 'data-mk')
    .map(a => [a.name, a.value])
    .sort();
  if (element instanceof HTMLInputElement) attrs.push(['(value)', element.value]);
  if (element instanceof HTMLOptionElement) attrs.push(['(selected)', String(element.selected)]);
  const children: unknown[] = [];
  let text = '';
  const unwrapped = (node: Node): Node[] =>
    node instanceof HTMLElement && node.localName === 'mk-island'
      ? [...node.childNodes].filter(c => !(c instanceof HTMLScriptElement)).flatMap(unwrapped)
      : [node];
  for (const child of [...element.childNodes].flatMap(unwrapped)) {
    if (child.nodeType === Node.TEXT_NODE) { text += child.textContent; continue; }
    if (child.nodeType !== Node.ELEMENT_NODE) continue;
    if (text) { children.push(text); text = ''; }
    children.push(canon(child));
  }
  if (text) children.push(text);
  return [element.tagName.toLowerCase(), attrs, children];
}

const results = fixtures.map(fixture => {
  const solidRoot = document.createElement('div');
  const rustRoot = document.createElement('div');
  // Without its behaviour's markup: what the server renders.
  const given = props(fixture.props as unknown as Json) as Record<string, unknown>;
  if (fixture.component in islands) given.enhanced = false;
  render(() => createComponent(registry[fixture.component], given), solidRoot);
  rustRoot.innerHTML = fixture.html;
  const solid = JSON.stringify(canon(solidRoot));
  const rust = JSON.stringify(canon(rustRoot));
  const section = document.createElement('section');
  section.dataset.component = fixture.component;
  section.append(solidRoot);
  document.querySelector('#examples')!.append(section);
  return { component: fixture.component, title: fixture.title, same: solid === rust, solid, rust };
});

(window as unknown as { equivalence: unknown }).equivalence = results;
