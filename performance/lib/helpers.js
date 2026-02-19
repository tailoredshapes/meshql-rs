import http from 'k6/http';
import { check } from 'k6';
import { BASE_URL, ENTITIES } from './config.js';

const JSON_HEADERS = { headers: { 'Content-Type': 'application/json' } };

export function restPost(entityKey, body) {
  const url = `${BASE_URL}${ENTITIES[entityKey].rest}`;
  const res = http.post(url, JSON.stringify(body), JSON_HEADERS);
  check(res, { [`POST ${entityKey} → 201`]: (r) => r.status === 201 });
  return res;
}

export function restGet(entityKey, id) {
  const url = `${BASE_URL}${ENTITIES[entityKey].rest}/${id}`;
  const res = http.get(url);
  check(res, { [`GET ${entityKey} → 200`]: (r) => r.status === 200 });
  return res;
}

export function restPut(entityKey, id, body) {
  const url = `${BASE_URL}${ENTITIES[entityKey].rest}/${id}`;
  const res = http.put(url, JSON.stringify(body), JSON_HEADERS);
  check(res, { [`PUT ${entityKey} → 200`]: (r) => r.status === 200 });
  return res;
}

export function restDelete(entityKey, id) {
  const url = `${BASE_URL}${ENTITIES[entityKey].rest}/${id}`;
  const res = http.del(url);
  check(res, { [`DELETE ${entityKey} → 200`]: (r) => r.status === 200 });
  return res;
}

export function graphqlQuery(entityKey, query) {
  const url = `${BASE_URL}${ENTITIES[entityKey].graph}`;
  const res = http.post(url, JSON.stringify({ query }), JSON_HEADERS);
  check(res, { [`GraphQL ${entityKey} → 200`]: (r) => r.status === 200 });
  if (res.status !== 200) return null;
  const parsed = res.json();
  if (parsed.errors) {
    console.error(`GraphQL errors: ${JSON.stringify(parsed.errors)}`);
  }
  return parsed.data;
}

export function discoverIds(entityKey, queryName) {
  const query = `{ ${queryName} { id } }`;
  const data = graphqlQuery(entityKey, query);
  if (!data || !data[queryName]) return [];
  const result = data[queryName];
  if (Array.isArray(result)) {
    return result.map((item) => item.id);
  }
  return result.id ? [result.id] : [];
}

export function discoverAll(entityKey, queryName, fields) {
  const query = `{ ${queryName} { ${fields} } }`;
  const data = graphqlQuery(entityKey, query);
  if (!data || !data[queryName]) return [];
  return Array.isArray(data[queryName]) ? data[queryName] : [data[queryName]];
}

export function randomItem(arr) {
  return arr[Math.floor(Math.random() * arr.length)];
}
