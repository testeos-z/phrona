# Integración de Phrona con Flowise

Guía operativa para conectar un **Custom MCP** de Flowise con el fork de
Phrona publicado en Railway:

```text
https://phrona-mcp-fork-dev.up.railway.app/mcp
```

La guía distingue entre hechos verificados, contratos del checkout actual y
comportamientos que pueden cambiar con la versión de Flowise. No requiere
modificar Phrona ni compilarlo.

## Ruta rápida

1. Confirmá `GET /health` y anotá si `auth` es `true` o `false`.
2. En Railway configurá `PHRONA_API_KEY`; no uses el endpoint público sin una
   clave en producción.
3. Guardá la clave en una variable de Flowise, no en el JSON del chatflow.
4. En un Agent agregá un **Custom MCP** con transporte **Streamable HTTP** y
   la URL terminada en `/mcp`.
5. Agregá un header `x-api-key` con `{{$vars.phronaApiKey}}` y refrescá
   **Available Actions**.
6. Seleccioná las acciones, guardá el nodo y probá primero `list_engines` y
   después una búsqueda.

## Estado y arquitectura

### Qué está conectado

Flowise actúa como host MCP; su Custom MCP mantiene una conexión cliente con el
servidor Phrona. El fork expone las mismas capacidades por dos superficies:

| Superficie | Uso | Estado de este despliegue |
| --- | --- | --- |
| `/mcp` | MCP sobre **Streamable HTTP** | URL operativa suministrada para Railway |
| `/health` | diagnóstico público | Verificado: `status: ok`, versión `0.2.0` |
| `/v1/*` | REST, no es la conexión de este documento | Ver [api.md](api.md) |

El endpoint MCP está montado exactamente en `/mcp`. No reemplaces la URL por la
raíz del dominio. Streamable HTTP usa mensajes HTTP y puede usar Server-Sent
Events; no es el transporte stdio local documentado en [mcp.md](mcp.md).

### Autenticación: estado actual y objetivo

La respuesta verificada de `/health` contiene:

```json
{"status":"ok","version":"0.2.0","engines":{"web":12,"images":6,"news":4,"videos":3,"books":1},"auth":false}
```

`auth:false` significa que el Railway actual está expuesto sin exigir clave.
Es un **riesgo**, no una recomendación ni una garantía para futuros despliegues.
El contrato del repo permite `PHRONA_API_KEY` y acepta, cuando está configurada:

```http
x-api-key: <clave>
```

o:

```http
Authorization: Bearer <clave>
```

La configuración recomendada para producción es:

1. Definir `PHRONA_API_KEY` como variable secreta de Railway.
2. Redeployar y comprobar que `/health` informe `"auth":true`.
3. Enviar la misma clave desde Flowise mediante `x-api-key` o Bearer.

El estado `auth:false` fue observado en el servicio actual. Esta guía **no
afirma que el código de `source_policy` del checkout esté desplegado en
Railway**; comprobalo con `tools/list` antes de usar esos campos.

## Requisitos previos

- Una instancia de Flowise con un Agent y un modelo configurado.
- Permiso para crear variables y editar el chatflow.
- Acceso administrativo al servicio Railway para configurar
  `PHRONA_API_KEY`.
- Salida HTTPS desde Flowise hacia `phrona-mcp-fork-dev.up.railway.app`.
- Un chatflow de prueba separado del chatflow de producción.

La documentación oficial consultada para Custom MCP y Streamable HTTP es:
[Flowise: Tools & MCP](https://github.com/flowiseai/flowisedocs/blob/main/en/tutorials/tools-and-mcp.md).
Flowise puede mover nombres o ubicación de controles entre versiones; si una
etiqueta no coincide, buscá la función equivalente dentro del Agent y del
Custom MCP, sin cambiar el contrato JSON.

## Configurar Custom MCP en Flowise

### 1. Crear la variable

Creá una variable de Flowise, por ejemplo `phronaApiKey`, con el valor secreto
de `PHRONA_API_KEY`. La forma exacta del panel para crear variables depende de
la versión de Flowise. La sintaxis documentada por Flowise para referenciarlas
en la configuración es `$vars.<nombre>` dentro de `{{ }}`.

No pegues la clave literal en el MCP Server Config ni en un prompt.

### 2. Agregar el Custom MCP

Dentro del Agent, agregá un Custom MCP. Para Streamable HTTP, el servidor se
configura con URL y headers. El objeto mínimo recomendado es:

```json
{
  "url": "https://phrona-mcp-fork-dev.up.railway.app/mcp",
  "headers": {
    "x-api-key": "{{$vars.phronaApiKey}}"
  }
}
```

Si el editor muestra explícitamente un selector de transporte, elegí
**Streamable HTTP**. Si la versión no muestra ese selector, la presencia de
`url` (en lugar de `command`/`args`) corresponde al flujo Streamable HTTP de la
documentación de Flowise.

Alternativa equivalente con Bearer:

```json
{
  "url": "https://phrona-mcp-fork-dev.up.railway.app/mcp",
  "headers": {
    "Authorization": "Bearer {{$vars.phronaApiKey}}"
  }
}
```

Usá una sola de las dos formas. `x-api-key` es la opción más simple para
Phrona; Bearer existe para clientes que estandarizan `Authorization`.

### 3. Descubrir y guardar acciones

1. Guardá o aplicá la configuración del Custom MCP.
2. Presioná **Refresh Available Actions**. El texto exacto puede variar según
   versión, pero la operación es refrescar las acciones disponibles del MCP.
3. Verificá que aparezcan las herramientas de Phrona:
   `web_search`, `image_search`, `news_search`, `video_search`, `book_search`,
   `suggest`, `fetch_page`, `search_grounded` y `list_engines`.
4. Seleccioná las acciones que el Agent puede utilizar.
5. Guardá nuevamente el nodo/chatflow y ejecutá una prueba.

Si cambiás URL, header, variable o permisos, reconectá el Custom MCP:

1. Actualizá la configuración.
2. Guardá/re-aplicá el nodo.
3. Volvé a refrescar **Available Actions**.
4. Si las acciones antiguas quedan en pantalla, quitá y agregá de nuevo el
   Custom MCP o reabrí el editor, según la versión.
5. Guardá el chatflow antes de probar.

Cada chatflow guarda su propia configuración del nodo. Cambiar la variable
global no agrega automáticamente el nodo a otros chatflows, y refrescar
acciones en uno no actualiza las acciones seleccionadas en los demás.

### Migrar varios chatflows sin romper producción

1. Duplicá cada chatflow y trabajá primero en la copia.
2. Creá o verificá la variable `phronaApiKey`.
3. Actualizá un único Custom MCP en la copia.
4. Guardá, refrescá acciones y ejecutá el smoke test de esta guía.
5. Compará las acciones seleccionadas y el prompt del Agent con producción.
6. Repetí la migración chatflow por chatflow.
7. Promové cada copia sólo después de probarla; no edites todos los
   chatflows a la vez.

## Herramientas y argumentos

El esquema actual del código MCP usa JSON. Todas las herramientas reciben sus
argumentos como un objeto; la salida es texto que contiene JSON.

### Regla crítica: `engines` es string

En MCP, `engines` es una **cadena separada por comas**, no un array:

```json
{"query":"Rust ownership","engines":"bing,brave","max_results":5}
```

No uses esto:

```json
{"query":"Rust ownership","engines":["bing","brave"]}
```

Si omitís `engines`, Phrona usa todos los engines habilitados para la
categoría. La lista exacta se obtiene con `list_engines`:

```json
{"category":"news"}
```

Respuesta esperada, si coincide con el estado observado:

```json
{"engines":{"news":["duckduckgo_news","bing_news","yahoo_news","brave_news"]}}
```

Los nombres están **acotados por categoría**. `google` es un engine web;
`google_images` sería un engine de imágenes si estuviera disponible. No pases
`google` a una búsqueda de noticias.

### Forma común de búsqueda

`web_search`, `image_search`, `news_search`, `video_search` y `book_search`
comparten esta forma, con campos opcionales:

```json
{
  "query":"texto requerido",
  "engines":"engine1,engine2",
  "max_results":5,
  "region":"us-en",
  "language":"en",
  "time_range":"week",
  "safesearch":"moderate",
  "filters":"site:example.org",
  "page":1,
  "source_policy":{
    "mode":"prefer-official",
    "allowed_domains":["example.org"],
    "excluded_domains":["private.example.org"]
  }
}
```

`source_policy` sólo está en el contrato actual del checkout para búsquedas y
`fetch_page`; verificá que aparezca en `tools/list` del Railway antes de
enviarlo.

Ejemplos válidos por categoría:

| Herramienta | Ejemplo mínimo | Engines válidos de referencia |
| --- | --- | --- |
| `web_search` | `{"query":"Rust language"}` | `duckduckgo`, `google`, `bing`, `brave` |
| `image_search` | `{"query":"Patagonia landscape","engines":"duckduckgo_images,google_images"}` | `duckduckgo_images`, `bing_images`, `brave_images`, `startpage_images`, `mojeek_images`, `google_images` |
| `news_search` | `{"query":"economía argentina","time_range":"week"}` | `duckduckgo_news`, `bing_news`, `yahoo_news`, `brave_news` |
| `video_search` | `{"query":"Rust conference","engines":"bing_videos,brave_videos"}` | `duckduckgo_videos`, `bing_videos`, `brave_videos` |
| `book_search` | `{"query":"distributed systems","engines":"annas_archive"}` | `annas_archive` |

Ejemplos explícitos de noticias:

```json
{"query":"elecciones Argentina","max_results":5}
```

```json
{"query":"climate policy","engines":"bing_news,brave_news","time_range":"month"}
```

La primera forma, sin `engines`, es la preferida. Los conteos y disponibilidad
son ambientales; `list_engines` es la fuente de verdad para ese despliegue.

### Otras herramientas

```json
{"query":"rust own"}
```

corresponde a `suggest`. `source` puede ser `duckduckgo`, `google`, `bing`,
`brave`, `startpage`, `qwant` o `wikipedia`.

```json
{
  "url":"https://www.rust-lang.org/learn",
  "max_chars":8000,
  "query":"ownership"
}
```

corresponde a `fetch_page`. `url` es obligatorio; `max_chars`, `query` y
`source_policy` son opcionales.

```json
{"query":"What is Rust ownership?","max_results":5}
```

corresponde a `search_grounded`. Devuelve `query`, `answer` y `sources`; cada
source contiene `title`, `url`, `content` y `score`, además de metadata de
política cuando el contrato desplegado la soporte.

## Política de fuentes

### Dos autoridades distintas

`allowed_domains` expresa el alcance que pidió el caller. No es una lista de
fuentes oficiales. La autoridad real viene del catálogo `sources` configurado
por el operador en Phrona:

| Campo de resultado | Significado |
| --- | --- |
| `requested_match` | El hostname coincide con lo solicitado por el caller |
| `source_tier` | Clasificación del catálogo del operador: `official`, `secondary` o `unknown` |
| `source_policy_mode` | Modo aplicado: `any`, `prefer-official`, `require-allowed` u `official-only` |
| `policy_reason` | Explicación local de admisión/exclusión |

El usuario no puede convertir un dominio en oficial escribiéndolo en
`allowed_domains`, poniendo `site:...` en `filters` o mencionando “oficial” en
la consulta. Tampoco hay que inferir officiality por texto de la URL.

### Modos

| Modo | Uso operativo |
| --- | --- |
| `any` | No pedir una restricción de autoridad; es el default |
| `prefer-official` | Priorizar autoridad oficial sin excluir todo lo demás |
| `require-allowed` | Exigir que el hostname esté en el alcance pedido, sin conferir tier oficial |
| `official-only` | Aceptar sólo hosts marcados `official` por el catálogo del operador |

Para investigación gubernamental, pedí primero un conjunto de organismos y
usá `official-only` sólo si el catálogo operativo realmente contiene esos
dominios:

```json
{
  "query":"programa de vacunación calendario 2026",
  "source_policy":{
    "mode":"official-only",
    "allowed_domains":["argentina.gob.ar","www.boletinoficial.gob.ar"],
    "excluded_domains":[]
  }
}
```

Para combinar fuentes primarias con análisis confiable, preferí oficiales y
solicitá explícitamente secundarios conocidos por el operador:

```json
{
  "query":"impacto económico de la medida X",
  "source_policy":{
    "mode":"prefer-official",
    "allowed_domains":["argentina.gob.ar","indec.gob.ar","university.example"],
    "excluded_domains":["spam.example"]
  }
}
```

Después inspeccioná `source_tier`, `requested_match` y `policy_reason` antes de
responder. Para una respuesta con citas, podés usar `search_grounded`; para
controlar el contenido fuente, encadená `web_search` y `fetch_page`:

```json
{"query":"inflación IPC metodología","source_policy":{"mode":"prefer-official","allowed_domains":["indec.gob.ar"],"excluded_domains":[]}}
```

```json
{"url":"https://www.indec.gob.ar/indec/web/Nivel4-Tema-3-5-31","query":"metodología IPC","source_policy":{"mode":"require-allowed","allowed_domains":["indec.gob.ar"],"excluded_domains":[]}}
```

Los nombres y dominios son ejemplos de entrada; la clasificación depende del
catálogo del operador y del despliegue. No afirmamos que esos dominios estén
cargados en Railway.

## Instrucciones para el Agent

Pegá instrucciones equivalentes en el prompt del Agent, ajustando el formato
que acepte tu versión de Flowise:

```text
Usá list_engines si necesitás confirmar disponibilidad.
Para búsquedas normales omití engines; si los pasás, engines DEBE ser un string
separado por comas, nunca un array.
No pases google a news_search: news usa nombres como bing_news o brave_news.
Usá source_policy sólo cuando el usuario pida una restricción de fuentes o una
jerarquía de autoridad.
Después de buscar, inspeccioná url, source_tier, requested_match y policy_reason.
No infieras que una fuente es oficial por el texto de su URL, dominio pedido,
filtro site: o afirmación del usuario.
Para una respuesta respaldada, usá search_grounded o fetch_page sobre una URL
devuelta por la búsqueda y citá el contenido obtenido.
```

Esto reduce errores de selección, pero no reemplaza la validación del esquema
que Flowise obtiene desde `tools/list`.

## Smoke test

### Desde Flowise

- [ ] El Custom MCP usa la URL exacta con `/mcp`.
- [ ] El header usa una variable, no un secreto literal.
- [ ] La variable resuelve a la misma clave configurada en Railway.
- [ ] **Available Actions** se refrescó después de guardar la configuración.
- [ ] Aparece `list_engines` y devuelve categorías.
- [ ] `news_search` funciona sin `engines`.
- [ ] `web_search` devuelve `results` y reporta `engines` por proveedor.
- [ ] `fetch_page` devuelve `url`, `title`, `description` y `text`.
- [ ] Si se prueba política, la acción expone `source_policy` y los resultados
  exponen metadata antes de usarla en producción.

No tomes una búsqueda exitosa como garantía de todos los proveedores: cada
engine depende de su disponibilidad externa.

### Directo contra MCP

La siguiente secuencia sirve para diagnóstico. Requiere `curl` y `jq` sólo si
querés formatear la salida. Si Railway tiene auth habilitada, agregá
`-H "x-api-key: $PHRONA_API_KEY"` a cada POST. No pongas la clave en la URL.

Salud:

```bash
curl -fsS https://phrona-mcp-fork-dev.up.railway.app/health
```

Inicialización y descubrimiento (los headers `Accept` son importantes para
Streamable HTTP):

```bash
curl -i -X POST https://phrona-mcp-fork-dev.up.railway.app/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke-test","version":"1.0"}}}'
```

Conservá el valor de `Mcp-Session-Id` si el servidor lo devuelve y usalo en
los POST siguientes:

```bash
curl -sS -X POST https://phrona-mcp-fork-dev.up.railway.app/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Mcp-Session-Id: <session-id>' \
  --data '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

Luego enviá `notifications/initialized` si el cliente lo requiere, y probá
las herramientas con `tools/call`:

```bash
curl -sS -X POST https://phrona-mcp-fork-dev.up.railway.app/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H 'Mcp-Session-Id: <session-id>' \
  --data '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"news_search","arguments":{"query":"Argentina"}}}'
```

Repetí con `web_search`, `fetch_page` y, sólo si aparece en `tools/list`, una
política:

```json
{"name":"web_search","arguments":{"query":"presupuesto público","source_policy":{"mode":"prefer-official","allowed_domains":["argentina.gob.ar"],"excluded_domains":[]}}}
```

Resultados esperados: respuestas JSON dentro del contenido MCP; las búsquedas
incluyen `query`, `total`, `results` y reportes `engines`; `fetch_page` incluye
contenido extraído. Google 429, CAPTCHA, páginas que bloquean fetch y ausencia
de resultados son condiciones ambientales, no fallas automáticas del
transporte.

## Troubleshooting

| Síntoma | Causa probable | Acción |
| --- | --- | --- |
| `isStreamValid:false` | URL, transporte o respuesta no corresponden a Streamable HTTP | Usá exactamente `/mcp`, seleccioná Streamable HTTP si existe y refrescá acciones; verificá `Accept` en un POST directo |
| `no engines available` | Categoría sin engines habilitados o names de otra categoría | Ejecutá `list_engines`; omití `engines` o usá nombres exactos de esa categoría |
| `invalid type: sequence, expected a string` | `engines` fue enviado como array | Cambiá `['bing','brave']` por `"bing,brave"` |
| Google 429/Captcha | Rate limit o desafío del proveedor | No lo trates como falla MCP; omití Google, usá engines disponibles y revisá el reporte `engines` |
| fetch 403 | El sitio destino bloquea el extractor, requiere sesión o deniega el proxy | Probá otra fuente autorizada; no desactives SSRF ni presentes el 403 como fallo de Flowise |
| `deployed:null` / `isPublic:null` | Metadatos del editor o instancia de Flowise, no una respuesta MCP | Verificá conectividad con `/health`, luego `/mcp` y la configuración del nodo; no inventes un estado de deploy |
| No aparecen acciones | Configuración no guardada, header inválido, sesión vieja o endpoint incorrecto | Guardá/re-aplicá, refrescá Available Actions, revisá variable/header y recreá el nodo sólo en la copia |
| API 401/403 | `PHRONA_API_KEY` está configurada y falta o no coincide el header | Usá `x-api-key` o `Authorization: Bearer`; comprobá la variable sin exponer su valor |

Si `tools/list` no incluye `source_policy`, no lo envíes aunque aparezca en el
checkout local. Eso indica que el despliegue todavía no contiene ese contrato
o que estás conectado a otra versión.

## Seguridad y producción

- [ ] Configurá `PHRONA_API_KEY` en Railway antes de exponer el MCP.
- [ ] Confirmá `/health` con `"auth":true` después del deploy.
- [ ] Usá variables de Flowise; nunca commitees claves, las pegues en prompts
  ni las pongas como query string.
- [ ] Limitá quién puede acceder al chatflow y al dominio público de Flowise.
- [ ] Considerá un proxy privado, allow-list de red o autenticación adicional
  si el caso de uso lo permite.
- [ ] Revisá CORS y las políticas del reverse proxy al colocar Flowise delante
  o detrás de otro dominio; el MCP no debe quedar accidentalmente abierto por
  una regla amplia.
- [ ] Recordá que `source_policy` es control local de alcance y autoridad; no
  es una prueba criptográfica de identidad del sitio.
- [ ] Duplicá chatflows antes de cambiar URL, headers, acciones o prompts.
- [ ] Rotá la clave si apareció en logs, exportaciones de chatflows o capturas.
- [ ] Monitoreá límites, errores de proveedor y respuestas 429/403 por separado
  de errores de transporte MCP.

## Referencias del repositorio

- [MCP server reference](mcp.md): transporte, herramientas y formato de salida.
- [REST API reference](api.md): autenticación, categorías, engines y metadata.
- [Web frontend](frontend.md): nombres de campos REST y diferencia con el
  objeto MCP anidado.
- [Configuración](../phrona.yaml): `PHRONA_API_KEY`, bind addresses, límites y
  catálogo `sources` del operador.
- [Flowise: Tools & MCP](https://github.com/flowiseai/flowisedocs/blob/main/en/tutorials/tools-and-mcp.md): Custom MCP, Streamable HTTP, variables, headers y refresh de Available Actions.

## Verificación de alcance

Este archivo es documentación operativa únicamente. No modifica la aplicación,
no actualiza Railway y no declara desplegado el nuevo código de política de
fuentes. Los hechos de Railway indicados fueron observados en
`https://phrona-mcp-fork-dev.up.railway.app/health`; la lista de herramientas,
argumentos y `source_policy` debe confirmarse contra `tools/list` del endpoint
al momento de integrar.
