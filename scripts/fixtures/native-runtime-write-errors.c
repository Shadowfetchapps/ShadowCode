#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fnmatch.h>
#include <ftw.h>
#include <limits.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// The filesystem reader is a one-file fixture; the tested extractor and cleanup
// functions below are copied mechanically, without edits, from the pinned source.
typedef int sqfs_err;
typedef int64_t sqfs_off_t;
#define SQFS_OK 0
#define SQUASHFS_DIR_TYPE 1
#define SQUASHFS_LDIR_TYPE 8
#define SQUASHFS_REG_TYPE 2
#define SQUASHFS_LREG_TYPE 9
#define SQUASHFS_SYMLINK_TYPE 3
#define SQUASHFS_LSYMLINK_TYPE 10
#define FNM_FILE_NAME FNM_PATHNAME
static off_t fs_offset;
static volatile sig_atomic_t extraction_signal;
typedef struct { struct { unsigned int inodes; } sb; int fd; } sqfs;
typedef struct { struct { int inode_type; unsigned int inode_number; } base;
                 struct { struct { off_t file_size; } reg; } xtra; } sqfs_inode;
typedef struct { bool dir_end; const char *path; struct { unsigned long inode; } entry; int next; } sqfs_traverse;
static unsigned char *fixture_bytes;
static const size_t fixture_size = 196731;
static unsigned int opened, closed, write_calls, read_calls;
static bool injecting;
static FILE *target_stream;
static const char *fault;

static int mkdir_p(const char *p) { return mkdir(p,0700)==0 || errno==EEXIST ? 0 : -1; }
static void die(const char *message) { fprintf(stderr,"fixture fatal: %s\n",message); abort(); }
static sqfs_err sqfs_open_image(sqfs *f,const char *p,size_t at) { (void)p;(void)at;f->sb.inodes=1;f->fd=-1;return 0; }
static unsigned long sqfs_inode_root(sqfs *f) { (void)f;return 1; }
static sqfs_err sqfs_traverse_open(sqfs_traverse *t,sqfs *f,unsigned long inode) { (void)f;(void)inode;memset(t,0,sizeof(*t));return 0; }
static bool sqfs_traverse_next(sqfs_traverse *t,sqfs_err *e) { *e=0;if(t->next++)return false;t->path="AppRun";t->entry.inode=1;return true; }
static sqfs_err sqfs_inode_get(sqfs *f,sqfs_inode *i,unsigned long n) { (void)f;(void)n;memset(i,0,sizeof(*i));i->base.inode_type=SQUASHFS_REG_TYPE;i->base.inode_number=1;i->xtra.reg.file_size=fixture_size;return 0; }
static int private_sqfs_stat(sqfs *f,sqfs_inode *i,struct stat *s) { (void)f;(void)i;memset(s,0,sizeof(*s));s->st_mode=S_IFREG|0700;s->st_size=fixture_size;return 0; }
static sqfs_err sqfs_read_range(sqfs *f,sqfs_inode *i,sqfs_off_t at,sqfs_off_t *size,void *buf) { (void)f;(void)i;read_calls++;if(at<0||(size_t)at>=fixture_size)return 1;if((size_t)*size>fixture_size-(size_t)at)*size=(sqfs_off_t)(fixture_size-(size_t)at);memcpy(buf,fixture_bytes+at,(size_t)*size);return 0; }
static sqfs_err sqfs_readlink(sqfs *f,sqfs_inode *i,char *buf,size_t *size) { (void)f;(void)i;(void)buf;*size=0;return 1; }
static void sqfs_traverse_close(sqfs_traverse *t) { (void)t; }
static void sqfs_fd_close(int fd) { (void)fd; }

FILE *__real_fopen(const char *,const char *);
size_t __real_fwrite(const void *,size_t,size_t,FILE *);
int __real_fflush(FILE *);
int __real_fclose(FILE *);
FILE *__wrap_fopen(const char *name,const char *mode) {
    FILE *f=__real_fopen(injecting && strncmp(fault,"dev_full",8)==0 ? "/dev/full" : name,mode);
    if(injecting && f) {
        target_stream=f;opened++;
        if(strcmp(fault,"dev_full_buffered")==0) assert(setvbuf(f,NULL,_IOFBF,262144)==0);
        if(strcmp(fault,"dev_full_unbuffered")==0) assert(setvbuf(f,NULL,_IONBF,0)==0);
    }
    return f;
}
size_t __wrap_fwrite(const void *data,size_t size,size_t count,FILE *f) {
    if(injecting && f==target_stream) {
        write_calls++;
        if(write_calls==2 && (strcmp(fault,"short_write")==0 || strcmp(fault,"short_and_close")==0)) {
            size_t result=__real_fwrite(data,size,count-1,f);errno=ENOSPC;return result;
        }
        if(write_calls==1 && strcmp(fault,"zero_write")==0) {errno=ENOSPC;return 0;}
    }
    return __real_fwrite(data,size,count,f);
}
int __wrap_fflush(FILE *f) {
    int result=__real_fflush(f);
    if(injecting && f==target_stream && strcmp(fault,"flush_error")==0) {errno=ENOSPC;return EOF;}
    return result;
}
int __wrap_fclose(FILE *f) {
    bool target=injecting && f==target_stream;
    if(target)closed++;
    int result=__real_fclose(f);
    if(target && (strcmp(fault,"close_error")==0 || strcmp(fault,"short_and_close")==0)) {errno=EIO;return EOF;}
    return result;
}

#include "extraction_under_test.inc"

static char *joined(const char *root,const char *leaf) {char *p=NULL;assert(asprintf(&p,"%s/%s",root,leaf)>0);return p;}
int main(int argc,char **argv) {
    assert(argc==3);fault=argv[1];
    char *root=joined(argv[2],"case-XXXXXX");assert(mkdtemp(root));
    char *dir=joined(root,"owned-XXXXXX");assert(mkdtemp(dir));
    char *peer=joined(root,"peer.txt");FILE *sentinel=fopen(peer,"w");assert(sentinel);assert(fputs("preserved",sentinel)>=0);assert(fclose(sentinel)==0);
    char *marker=joined(root,"payload-ran");assert(setenv("SHADOW_TEST_PAYLOAD_MARKER",marker,1)==0);
    fixture_bytes=malloc(fixture_size);assert(fixture_bytes);memset(fixture_bytes,'x',fixture_size);
    const char *script="#!/bin/sh\nprintf ran > \"$SHADOW_TEST_PAYLOAD_MARKER\"\n#";
    memcpy(fixture_bytes,script,strlen(script));fixture_bytes[fixture_size-1]='\n';
    if(strcmp(fault,"cancelled")==0)extraction_signal=SIGTERM;
    injecting=true;
    bool extracted=extract_appimage("one-file-fixture",dir,NULL,false,false);
    injecting=false;
    // This is the same Boolean execution gate used by extract-and-run. Actual
    // payload execution is recorded independently of whether its file is intact.
    int attempts=0,payload_status=-1;
    if(extracted) {
        attempts++;
        pid_t pid=fork();assert(pid>=0);
        if(pid==0){char *exe=joined(dir,"AppRun");execl(exe,exe,(char *)NULL);_exit(127);}
        assert(waitpid(pid,&payload_status,0)==pid);
    }
    bool payload_ran=access(marker,F_OK)==0;
    bool cleanup=rm_recursive(dir);
    struct stat st;bool removed=lstat(dir,&st)<0 && errno==ENOENT;
    bool peer_ok=lstat(peer,&st)==0 && st.st_size==9;
    bool success=strcmp(fault,"success")==0;
    bool expected=extracted==success && attempts==(success?1:0) && payload_ran==success &&
        (!success || (WIFEXITED(payload_status)&&WEXITSTATUS(payload_status)==0)) &&
        opened==closed && closed==(strcmp(fault,"cancelled")==0?0u:1u) && cleanup && removed && peer_ok;
    printf("{\"case\":\"%s\",\"pass\":%s,\"extracted\":%s,\"payload_attempts\":%d,\"payload_ran\":%s,\"opened\":%u,\"closed\":%u,\"read_calls\":%u,\"write_calls\":%u,\"owned_removed\":%s,\"peer_preserved\":%s}\n",fault,expected?"true":"false",extracted?"true":"false",attempts,payload_ran?"true":"false",opened,closed,read_calls,write_calls,removed?"true":"false",peer_ok?"true":"false");
    assert(rm_recursive(root));
    free(root);free(dir);free(peer);free(marker);free(fixture_bytes);
    return expected?0:1;
}
