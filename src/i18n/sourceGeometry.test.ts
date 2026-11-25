import { expect, it } from 'vitest';
import i18n from 'i18next';
import './index';

it.each([1, 2, 5, 12, 22])('labels the bulk crop action without an incorrect plural for %i photos', (n) => {
  expect(i18n.t('session.toasts.sourceGeometryAll', { lng: 'en', n })).toBe(
    `Apply Capture One straightening and replace crops using each frame’s aspect ratio (Photos: ${n})`
  );
  expect(i18n.t('session.toasts.sourceGeometryAll', { lng: 'pl', n })).toBe(
    `Zastosuj prostowanie z Capture One i zastąp kadry z zachowaniem proporcji każdego obrazu (Liczba zdjęć: ${n})`
  );
});
