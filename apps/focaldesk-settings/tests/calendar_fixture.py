"""Isolated connector-host IPC and browser launcher; never contacts providers."""
import copy
from datetime import datetime,timedelta,timezone
import json
from pathlib import Path
import socket
import threading

class CalendarFixture:
    def __init__(self,root:Path):
        root.mkdir(parents=True)
        self.root=root
        self.path=root/'connectors.sock'
        self.calls=[]
        self.flows={}
        self.reject=False
        self.offline=False
        self.race_preferences=False
        self.bad_url=False
        self.syncs=0
        self.accounts=[{'account_id':'work','provider':'google','connected':True,'enabled':True,'cached_events':2,'sync_state':'idle','calendar_ids':['primary','work'],'reminder_minutes':10,'last_sync_at_unix':int(datetime.now(timezone.utc).timestamp()),'last_error':None}]
        start=datetime.now(timezone.utc)+timedelta(hours=1)
        self.events=[{'account_id':'work','provider':'google','calendar_id':'primary','event_id':'event-1','title':'Design review <team>','start_time':start.isoformat(),'end_time':(start+timedelta(hours=1)).isoformat(),'organizer':'Team','all_day':False}]
        self.server=socket.socket(socket.AF_UNIX)
        self.server.bind(str(self.path))
        self.server.listen()
        threading.Thread(target=self.serve,daemon=True).start()
        binary=root/'bin'
        binary.mkdir()
        opener=binary/'xdg-open'
        opener.write_text('#!/usr/bin/env python3\nimport os,json,sys\nfrom pathlib import Path\nroot=Path(os.environ["FOCALDESK_CALENDAR_FIXTURE"])\nwith (root/"browser.jsonl").open("a") as log:log.write(json.dumps({"url":sys.argv[1]})+"\\n")\n')
        opener.chmod(0o755)
        self.binary=binary

    def response(self,request):
        safe=copy.deepcopy(request)
        if safe.get('client_secret'): safe['client_secret']='[redacted]'
        self.calls.append(safe)
        result={'status':'ok','paused':False,'connectors':[],'calendar_accounts':[],'calendar_events':[],'available_calendars':[]}
        if self.offline: return {**result,'status':'error','message':'Fixture calendar service unavailable'}
        command=request['command']
        if command=='calendar_accounts': result['calendar_accounts']=copy.deepcopy(self.accounts)
        elif command=='calendar_upcoming': result['calendar_events']=copy.deepcopy(self.events)
        elif command=='calendar_discover':
            account=next(account for account in self.accounts if account['account_id']==request['account_id'])
            result['available_calendars']=[{'id':identifier,'name':name,'primary':identifier=='primary','selected':identifier in account['calendar_ids']} for identifier,name in [('primary','Personal'),('work','Work calendar'),('holidays','Holidays')]]
        elif command=='calendar_update_preferences_checked':
            account=next(account for account in self.accounts if account['account_id']==request['account_id'])
            if self.race_preferences:
                self.race_preferences=False
                account['reminder_minutes']=30
            if self.reject: return {**result,'status':'error','message':'Fixture rejected calendar preferences'}
            if sorted(account['calendar_ids'])!=sorted(request['expected_calendar_ids']) or account['reminder_minutes']!=request['expected_reminder_minutes']:
                return {**result,'status':'error','message':'Calendar preferences changed in another client. Reload calendars before applying.'}
            account.update(calendar_ids=request['calendar_ids'],reminder_minutes=request['reminder_minutes'])
        elif command=='calendar_sync_now':
            self.syncs+=1
            next(account for account in self.accounts if account['account_id']==request['account_id'])['sync_state']='queued'
        elif command=='calendar_disconnect':
            account=next(account for account in self.accounts if account['account_id']==request['account_id'])
            account.update(connected=False,enabled=False)
            if request['delete_local_data']:
                self.events=[event for event in self.events if event['account_id']!=request['account_id']]
                account['cached_events']=0
        elif command=='calendar_connect':
            identifier='fixture-flow-'+str(len(self.flows)+1)
            uri='https://accounts.google.com/o/oauth2/v2/auth?code_challenge=fixture&state=fixture-state' if request['provider']=='google' else 'https://login.microsoftonline.com/common/oauth2/v2.0/authorize?code_challenge=fixture'
            if self.bad_url: uri='file:///tmp/not-a-sign-in'
            flow={'flow_id':identifier,'provider':request['provider'],'state':'waiting_for_browser','verification_uri':uri,'message':'Finish sign-in','user_code':None}
            self.flows[identifier]={'public':flow,'account_id':request['account_id'],'polls':0}
            result['oauth']=copy.deepcopy(flow)
        elif command=='calendar_o_auth_status':
            # serde's snake_case spelling for CalendarOAuthStatus.
            flow=self.flows[request['flow_id']]
            flow['polls']+=1
            result['oauth']=copy.deepcopy(flow['public'])
        else: return {**result,'status':'error','message':'Unsupported fixture calendar request: '+command}
        return result

    def complete(self,identifier,success=True):
        flow=self.flows[identifier]
        flow['public']['state']='connected' if success else 'error'
        if not success:flow['public']['message']='access_token=must-not-reach-the-webview'
        if success:self.accounts.append({'account_id':flow['account_id'],'provider':flow['public']['provider'],'connected':True,'enabled':True,'cached_events':0,'sync_state':'waiting_for_consent','calendar_ids':['primary'],'reminder_minutes':10,'last_error':None})

    def serve(self):
        while True:
            connection,_=self.server.accept()
            with connection:
                chunks=[]
                while block:=connection.recv(65536):chunks.append(block)
                request=json.loads(b''.join(chunks))['payload']
                response=self.response(request)
                connection.sendall(json.dumps({'protocol_version':1,'payload':response}).encode())
