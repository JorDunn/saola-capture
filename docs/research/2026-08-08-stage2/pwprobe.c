/* pwprobe.c — minimal PipeWire video consumer used ONLY to answer
 * "is shm negotiable against niri's Mutter.ScreenCast node?"
 *
 * Throwaway Stage-2 research prototype. Lives in the scratch dir, never in the repo.
 *
 *   ./pwprobe <node-id> <mode>
 *      mode = shm      : EnumFormat has NO modifier property; Buffers dataType = MemFd|MemPtr
 *      mode = dmabuf   : EnumFormat advertises modifier LINEAR (DONT_FIXATE); dataType = DmaBuf
 *      mode = any      : EnumFormat has no modifier; dataType = MemFd|MemPtr|DmaBuf
 *
 * Prints: negotiated SPA_PARAM_Format, the Buffers param we replied with, and for the
 * first frames the actual spa_data type/fd/size the producer handed us.
 */
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <signal.h>
#include <sys/mman.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <linux/dma-buf.h>
#include <errno.h>

#include <pipewire/pipewire.h>
#include <spa/param/video/format-utils.h>
#include <spa/debug/format.h>
#include <spa/debug/pod.h>

#define DRM_FORMAT_MOD_LINEAR 0ULL

struct probe {
	struct pw_main_loop *loop;
	struct pw_stream *stream;
	struct spa_video_info format;
	int mode;               /* 0 shm, 1 dmabuf, 2 any */
	int frames;
	int got_format;
};

static const char *datatype_name(uint32_t t)
{
	switch (t) {
	case SPA_DATA_MemPtr:   return "MemPtr (shm, mapped pointer)";
	case SPA_DATA_MemFd:    return "MemFd (shm, memfd)";
	case SPA_DATA_DmaBuf:   return "DmaBuf";
	case SPA_DATA_MemId:    return "MemId";
	default:                return "Invalid/unknown";
	}
}

static void on_state_changed(void *udata, enum pw_stream_state old,
			     enum pw_stream_state state, const char *error)
{
	struct probe *p = udata;
	printf("[state] %s -> %s%s%s\n", pw_stream_state_as_string(old),
	       pw_stream_state_as_string(state), error ? " : " : "", error ? error : "");
	fflush(stdout);
	if (state == PW_STREAM_STATE_ERROR)
		pw_main_loop_quit(p->loop);
}

static void on_param_changed(void *udata, uint32_t id, const struct spa_pod *param)
{
	struct probe *p = udata;
	uint8_t buf[2048];
	struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buf, sizeof(buf));
	const struct spa_pod *params[1];
	uint32_t datatypes;

	if (param == NULL) {
		printf("[param] id=%u -> NULL (cleared)\n", id);
		fflush(stdout);
		return;
	}
	if (id != SPA_PARAM_Format)
		return;

	if (spa_format_parse(param, &p->format.media_type, &p->format.media_subtype) < 0)
		return;
	if (p->format.media_type != SPA_MEDIA_TYPE_video ||
	    p->format.media_subtype != SPA_MEDIA_SUBTYPE_raw)
		return;
	spa_format_video_raw_parse(param, &p->format.info.raw);
	p->got_format = 1;

	printf("[format] NEGOTIATED SPA_PARAM_Format:\n");
	spa_debug_format(4, NULL, param);
	printf("[format] parsed: format=%s size=%ux%u framerate=%u/%u modifier=0x%llx\n",
	       spa_debug_type_find_short_name(spa_type_video_format, p->format.info.raw.format),
	       p->format.info.raw.size.width, p->format.info.raw.size.height,
	       p->format.info.raw.framerate.num, p->format.info.raw.framerate.denom,
	       (unsigned long long)p->format.info.raw.modifier);
	fflush(stdout);

	switch (p->mode) {
	case 0:  datatypes = (1u << SPA_DATA_MemFd) | (1u << SPA_DATA_MemPtr); break;
	case 1:  datatypes = (1u << SPA_DATA_DmaBuf); break;
	case 3:  datatypes = (1u << SPA_DATA_MemFd) | (1u << SPA_DATA_MemPtr); break;
	default: datatypes = (1u << SPA_DATA_MemFd) | (1u << SPA_DATA_MemPtr) |
			     (1u << SPA_DATA_DmaBuf); break;
	}

	int stride = p->format.info.raw.size.width * 4;
	int size = stride * p->format.info.raw.size.height;

	params[0] = spa_pod_builder_add_object(&b,
		SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
		SPA_PARAM_BUFFERS_buffers,  SPA_POD_CHOICE_RANGE_Int(4, 2, 16),
		SPA_PARAM_BUFFERS_blocks,   SPA_POD_Int(1),
		SPA_PARAM_BUFFERS_size,     SPA_POD_Int(size),
		SPA_PARAM_BUFFERS_stride,   SPA_POD_Int(stride),
		SPA_PARAM_BUFFERS_dataType, SPA_POD_CHOICE_FLAGS_Int(datatypes));

	printf("[buffers] replying with SPA_PARAM_Buffers dataType mask 0x%x "
	       "(MemFd=%d MemPtr=%d DmaBuf=%d) size=%d stride=%d\n",
	       datatypes, !!(datatypes & (1u << SPA_DATA_MemFd)),
	       !!(datatypes & (1u << SPA_DATA_MemPtr)),
	       !!(datatypes & (1u << SPA_DATA_DmaBuf)), size, stride);
	fflush(stdout);

	pw_stream_update_params(p->stream, params, 1);
}

static void on_process(void *udata)
{
	struct probe *p = udata;
	struct pw_buffer *b;
	struct spa_buffer *sb;

	if ((b = pw_stream_dequeue_buffer(p->stream)) == NULL)
		return;
	sb = b->buffer;

	if (p->frames < 3) {
		printf("[frame %d] n_datas=%u\n", p->frames, sb->n_datas);
		for (uint32_t i = 0; i < sb->n_datas; i++) {
			struct spa_data *d = &sb->datas[i];
			printf("   data[%u] type=%u (%s) fd=%ld mapoffset=%u maxsize=%u "
			       "data=%p chunk{offset=%u size=%u stride=%d}\n",
			       i, d->type, datatype_name(d->type), (long)d->fd,
			       d->mapoffset, d->maxsize, d->data,
			       d->chunk->offset, d->chunk->size, d->chunk->stride);
			if (d->type == SPA_DATA_MemPtr && d->data != NULL && d->chunk->size >= 16) {
				unsigned char *px = (unsigned char *)d->data + d->mapoffset;
				printf("   first 4 px (B G R X): ");
				for (int k = 0; k < 16; k += 4)
					printf("[%02x %02x %02x %02x] ", px[k], px[k+1], px[k+2], px[k+3]);
				printf("\n");
			}
			if (d->type == SPA_DATA_DmaBuf && d->fd >= 0) {
				/* THE conservative-path question: can a LINEAR dmabuf be mmap'd
				 * and read as plain BGRx, with no GBM/EGL import? */
				off_t fdsize = lseek((int)d->fd, 0, SEEK_END);
				size_t want = (size_t)(p->format.info.raw.size.height) *
					      (size_t)(p->format.info.raw.size.width * 4);
				size_t maplen = (fdsize > 0 && (size_t)fdsize < want) ? (size_t)fdsize : want;
				void *m = mmap(NULL, maplen, PROT_READ, MAP_SHARED, (int)d->fd, 0);
				printf("   dmabuf fd size (lseek END) = %ld, want %zu, mmap -> %s\n",
				       (long)fdsize, want, m == MAP_FAILED ? strerror(errno) : "OK");
				if (m != MAP_FAILED) {
					struct dma_buf_sync sy = { .flags = DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ };
					int sr = ioctl((int)d->fd, DMA_BUF_IOCTL_SYNC, &sy);
					unsigned char *px = (unsigned char *)m;
					printf("   DMA_BUF_IOCTL_SYNC(START|READ) -> %d (%s)\n",
					       sr, sr == 0 ? "ok" : strerror(errno));
					printf("   px[0..3] (B G R X): ");
					for (int k = 0; k < 16; k += 4)
						printf("[%02x %02x %02x %02x] ", px[k], px[k+1], px[k+2], px[k+3]);
					printf("\n   px at row %u col %u: ",
					       p->format.info.raw.size.height / 2,
					       p->format.info.raw.size.width / 2);
					size_t off = (size_t)(p->format.info.raw.size.height / 2) *
						     (size_t)(p->format.info.raw.size.width * 4) +
						     (size_t)(p->format.info.raw.size.width / 2) * 4;
					if (off + 4 <= maplen)
						printf("[%02x %02x %02x %02x]", px[off], px[off+1], px[off+2], px[off+3]);
					printf("\n");
					/* non-black check across the whole mapping */
					size_t nonzero = 0;
					for (size_t k = 0; k < maplen; k += 4096)
						if (px[k] || px[k+1] || px[k+2]) nonzero++;
					printf("   non-black sample points: %zu / %zu\n",
					       nonzero, (maplen + 4095) / 4096);
					sy.flags = DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ;
					ioctl((int)d->fd, DMA_BUF_IOCTL_SYNC, &sy);
					munmap(m, maplen);
				}
			}
		}
		fflush(stdout);
	}
	p->frames++;
	pw_stream_queue_buffer(p->stream, b);
	if (p->frames >= 10)
		pw_main_loop_quit(p->loop);
}

static const struct pw_stream_events stream_events = {
	PW_VERSION_STREAM_EVENTS,
	.state_changed = on_state_changed,
	.param_changed = on_param_changed,
	.process = on_process,
};

static void on_timeout(void *udata, uint64_t expirations)
{
	struct probe *p = udata;
	printf("[timeout] giving up after 6s (got_format=%d frames=%d)\n", p->got_format, p->frames);
	fflush(stdout);
	pw_main_loop_quit(p->loop);
}

int main(int argc, char *argv[])
{
	struct probe p = { 0 };
	const struct spa_pod *params[1];
	uint8_t buf[2048];
	struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buf, sizeof(buf));
	uint32_t node_id;

	if (argc < 3) {
		fprintf(stderr, "usage: %s <node-id> <shm|dmabuf|any>\n", argv[0]);
		return 2;
	}
	node_id = (uint32_t)atoi(argv[1]);
	if (!strcmp(argv[2], "shm")) p.mode = 0;
	else if (!strcmp(argv[2], "dmabuf")) p.mode = 1;
	else if (!strcmp(argv[2], "modshm")) p.mode = 3;
	else p.mode = 2;

	pw_init(&argc, &argv);
	printf("== pwprobe: node=%u mode=%s  (libpipewire %s, headers %s)\n",
	       node_id, argv[2], pw_get_library_version(), pw_get_headers_version());

	p.loop = pw_main_loop_new(NULL);
	p.stream = pw_stream_new_simple(
		pw_main_loop_get_loop(p.loop), "saola-capture-stage2-probe",
		pw_properties_new(PW_KEY_MEDIA_TYPE, "Video",
				  PW_KEY_MEDIA_CATEGORY, "Capture",
				  PW_KEY_MEDIA_ROLE, "Screen", NULL),
		&stream_events, &p);

	if (p.mode == 1 || p.mode == 3) {
		/* dmabuf: advertise the modifier property as a DONT_FIXATE enum choice
		 * containing LINEAR only — the conservative mmap-able path. */
		struct spa_pod_frame f[2];
		spa_pod_builder_push_object(&b, &f[0], SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat);
		spa_pod_builder_add(&b,
			SPA_FORMAT_mediaType,    SPA_POD_Id(SPA_MEDIA_TYPE_video),
			SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
			SPA_FORMAT_VIDEO_format, SPA_POD_Id(SPA_VIDEO_FORMAT_BGRx), 0);
		spa_pod_builder_prop(&b, SPA_FORMAT_VIDEO_modifier,
				     SPA_POD_PROP_FLAG_MANDATORY | SPA_POD_PROP_FLAG_DONT_FIXATE);
		spa_pod_builder_push_choice(&b, &f[1], SPA_CHOICE_Enum, 0);
		spa_pod_builder_long(&b, DRM_FORMAT_MOD_LINEAR);  /* default */
		spa_pod_builder_long(&b, DRM_FORMAT_MOD_LINEAR);  /* alternative 1 */
		spa_pod_builder_pop(&b, &f[1]);
		spa_pod_builder_add(&b,
			SPA_FORMAT_VIDEO_size,
			SPA_POD_CHOICE_RANGE_Rectangle(&SPA_RECTANGLE(1920, 1080),
						       &SPA_RECTANGLE(1, 1),
						       &SPA_RECTANGLE(8192, 8192)),
			SPA_FORMAT_VIDEO_framerate,
			SPA_POD_CHOICE_RANGE_Fraction(&SPA_FRACTION(60, 1),
						      &SPA_FRACTION(0, 1),
						      &SPA_FRACTION(1000, 1)), 0);
		params[0] = spa_pod_builder_pop(&b, &f[0]);
	} else {
		/* shm / any: NO modifier property at all — this is the question. */
		params[0] = spa_pod_builder_add_object(&b,
			SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
			SPA_FORMAT_mediaType,    SPA_POD_Id(SPA_MEDIA_TYPE_video),
			SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
			SPA_FORMAT_VIDEO_format, SPA_POD_Id(SPA_VIDEO_FORMAT_BGRx),
			SPA_FORMAT_VIDEO_size,
			SPA_POD_CHOICE_RANGE_Rectangle(&SPA_RECTANGLE(1920, 1080),
						       &SPA_RECTANGLE(1, 1),
						       &SPA_RECTANGLE(8192, 8192)),
			SPA_FORMAT_VIDEO_framerate,
			SPA_POD_CHOICE_RANGE_Fraction(&SPA_FRACTION(60, 1),
						      &SPA_FRACTION(0, 1),
						      &SPA_FRACTION(1000, 1)));
	}

	printf("== offered SPA_PARAM_EnumFormat:\n");
	spa_debug_pod(4, NULL, params[0]);
	fflush(stdout);

	int res = pw_stream_connect(p.stream, PW_DIRECTION_INPUT, node_id,
				    PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS,
				    params, 1);
	printf("== pw_stream_connect -> %d\n", res);
	fflush(stdout);

	struct spa_source *timer = pw_loop_add_timer(pw_main_loop_get_loop(p.loop), on_timeout, &p);
	struct timespec ts = { .tv_sec = 6, .tv_nsec = 0 };
	pw_loop_update_timer(pw_main_loop_get_loop(p.loop), timer, &ts, NULL, false);

	pw_main_loop_run(p.loop);

	printf("== RESULT: got_format=%d frames_received=%d\n", p.got_format, p.frames);
	pw_stream_destroy(p.stream);
	pw_main_loop_destroy(p.loop);
	pw_deinit();
	return 0;
}
