# Provider plugins

# Problem

We want to have unified interface for working with multiple different llm providers — OpenAI, Anthropic, Google, etc. In order to achieve this there should be a facade layer that hides individual provider implementation.

# Proposal

Logic for working with individual providers is isolated in provider plugins. Provider plugins are implemented with ModKit plugin system. These plugins expose a trait that covers full superset of provider capabilities necessary to support all llmgw endpoints.

# Design

## Main prediction logic

Invoke llm eval by providing something very similar to original user request. Receive something very similar to our api response.

Async jobs are also scheduled here because they accept exactly the same input and for some providers we can't tell if the job will be async or not in advance.

```rust
async fn run(
    &self,
    provider_model_name: String,
    model_info, // received from ModelRegistry
    request, // message history, prediction parameters, tools
    is_async
) -> (provider_job_id, Optional<Response>) // provider id, full response with usage
```

## Streaming

Streaming should yield chunks asynchronously. Input is the same as in main predict. Streaming for incoming request should probably be run in some background worker to survive user disconnect, but provider plugin shouldn't care about this.

```rust
async fn stream(
    &self,
    provider_model_name: String,
    model_info, // received from ModelRegistry
    request // message history, prediction parameters, tools
) -> Stream<ResponseChunk>
```

## Embedding

```rust
async fn embed(
    &self,
    provider_model_name: String,
    model_info, // received from ModelRegistry
    embedding_request // array of strings to embed, parameters
) -> Embedding
```

## File processing

Do we need to download files/images first? Maybe not, as they can be forwarded as urls sometimes.

In view of missing file storage module we can't accept file storage urls. But we can still accept base64/external urls and forward them to providers. This logic should follow normal responses api format.

## Async jobs

For simulated async jobs we use run function in background.

For normal async jobs we should be able to retrieve status

```rust
async fn get_job_result(
    &self,
    provider_job_id
) -> (status, Optional<Response>)
```

## Returning images

Postponing this until file storage is ready.

## Batch jobs

Batch jobs accept file to upload and return batch job id. They also provide a way to check for job status and download the file back.

```rust
async fn schedule_batch_job(
    &self,
    file
) -> provider_batch_job_id

async fn get_batch_job_status(
    &self,
    provider_batch_job_id
) -> BatchJobResult // has status and file?

async fn get_batch_job_file(
    &self,
    provider_batch_job_id
) -> File
```

## Provider capabilities

We need to have a way to identify if provider plugin is capable of certain functionality. Do async jobs need to be emulated? Does blocking prediction needs to be emulated? Does it support batches? etc.

This capabilities change how main llmgw module works with provider plugins.

Naive approach - a function that returns some struct with capabilities.

TODO: define